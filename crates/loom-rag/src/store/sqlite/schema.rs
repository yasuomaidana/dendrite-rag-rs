//! The SQLite ledger's DDL, connection settings and schema versions.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};

use super::SqliteResultExt;
use crate::ledger::{LedgerError, SCHEMA_VERSION};

/// DDL version, recorded in `PRAGMA user_version`. A newer file is refused.
pub(super) const DDL_VERSION: i32 = 1;

/// Settings of every connection.
const SETTINGS: &str = "
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = OFF;
PRAGMA busy_timeout = 5000;
";

/// The tables mirror the redb tables; a file's `entry`, a chunk's `record` and a
/// community's `record` are JSON, and an embedding's `vec` is little-endian f32.
const DDL: &str = "
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, entry TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY, path TEXT NOT NULL, content_hash TEXT NOT NULL, record TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks (path);
CREATE INDEX IF NOT EXISTS chunks_content_hash ON chunks (content_hash);
CREATE TABLE IF NOT EXISTS embeddings (
    model TEXT NOT NULL, hash TEXT NOT NULL, vec BLOB NOT NULL, PRIMARY KEY (model, hash));
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS community_files (path TEXT PRIMARY KEY, cid INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS community_chunks (chunk TEXT PRIMARY KEY, cid INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS communities (id INTEGER PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS llm_cache (
    model TEXT NOT NULL, prompt_hash TEXT NOT NULL, value TEXT NOT NULL,
    PRIMARY KEY (model, prompt_hash));
CREATE TABLE IF NOT EXISTS concepts (path TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Opens (creating it if needed) the ledger file for writing: WAL mode, DDL at
/// [`DDL_VERSION`], and the content schema at [`SCHEMA_VERSION`].
pub(super) fn open_writer(path: &Path) -> crate::ledger::Result<Connection> {
    let mut conn = Connection::open(path).or_backend()?;
    let version = user_version(&conn)?;
    if version > DDL_VERSION {
        return Err(LedgerError::Backend(format!(
            "sqlite ledger DDL version {version} is newer than this loom-rag ({DDL_VERSION})"
        )));
    }
    conn.execute_batch("PRAGMA journal_mode = WAL;").or_backend()?;
    conn.execute_batch(SETTINGS).or_backend()?;
    if version < DDL_VERSION {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .or_backend()?;
        tx.execute_batch(DDL).or_backend()?;
        tx.execute_batch(&format!("PRAGMA user_version = {DDL_VERSION}"))
            .or_backend()?;
        tx.commit().or_backend()?;
    }
    check_content_schema(&mut conn)?;
    Ok(conn)
}

/// Opens a read-only connection to a ledger file that [`open_writer`] has set up.
pub(super) fn open_reader(path: &Path) -> crate::ledger::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_backend()?;
    conn.execute_batch(SETTINGS).or_backend()?;
    conn.execute_batch("PRAGMA query_only = ON;").or_backend()?;
    Ok(conn)
}

fn user_version(conn: &Connection) -> crate::ledger::Result<i32> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .or_backend()
}

/// An older content schema is cleared in one transaction (embeddings and the LLM cache are
/// kept) so the next index run re-chunks every file, as the redb ledger does; then the
/// current version is stored.
fn check_content_schema(conn: &mut Connection) -> crate::ledger::Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .or_backend()?;
    let stored: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| r.get(0))
        .optional()
        .or_backend()?;
    if stored.is_some_and(|v| v != SCHEMA_VERSION) {
        tx.execute_batch(
            "DELETE FROM meta WHERE key = 'merkle_root';
             DELETE FROM files;
             DELETE FROM chunks;
             DELETE FROM concepts;",
        )
        .or_backend()?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema', ?1)",
        [SCHEMA_VERSION],
    )
    .or_backend()?;
    tx.commit().or_backend()
}
