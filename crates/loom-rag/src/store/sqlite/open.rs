//! The SQLite ledger's DDL, connection settings and schema versions.

use std::path::Path;

use diesel::prelude::*;
use diesel::result::Error as DbError;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

use super::schema::{chunks, concepts, files, meta};
use super::{SqliteResultExt, backend, raw};
use crate::ledger::{LedgerError, SCHEMA_VERSION};

/// DDL version, recorded in `PRAGMA user_version`. A newer file is refused.
pub(super) const DDL_VERSION: i32 = 1;

/// The tables, as embedded Diesel migrations (`migrations/sqlite`). They are idempotent, so
/// a file created before Diesel (same DDL, no migration table) is simply brought under them.
const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/sqlite");

/// Opens (creating it if needed) the ledger file for writing: WAL mode, DDL at
/// [`DDL_VERSION`], and the content schema at [`SCHEMA_VERSION`].
pub(super) fn open_writer(path: &Path) -> crate::ledger::Result<SqliteConnection> {
    let mut conn = connect(path)?;
    let version = raw::user_version(&mut conn).or_backend()?;
    if version > DDL_VERSION {
        return Err(LedgerError::Backend(format!(
            "sqlite ledger DDL version {version} is newer than this loom-rag ({DDL_VERSION})"
        )));
    }
    raw::wal(&mut conn).or_backend()?;
    raw::settings(&mut conn).or_backend()?;
    if version < DDL_VERSION {
        conn.immediate_transaction::<_, DbError, _>(|tx| {
            tx.run_pending_migrations(MIGRATIONS)
                .map_err(DbError::QueryBuilderError)?;
            raw::set_user_version(tx, DDL_VERSION)
        })
        .or_backend()?;
    }
    check_content_schema(&mut conn)?;
    Ok(conn)
}

/// Opens a read connection to a ledger file that [`open_writer`] has set up; it refuses writes.
pub(super) fn open_reader(path: &Path) -> crate::ledger::Result<SqliteConnection> {
    let mut conn = connect(path)?;
    raw::settings(&mut conn).or_backend()?;
    raw::query_only(&mut conn).or_backend()?;
    Ok(conn)
}

fn connect(path: &Path) -> crate::ledger::Result<SqliteConnection> {
    let path = path
        .to_str()
        .ok_or_else(|| backend("the ledger path is not valid UTF-8"))?;
    SqliteConnection::establish(path).map_err(backend)
}

/// An older content schema is cleared in one transaction (embeddings and the LLM cache are
/// kept) so the next index run re-chunks every file, as the redb ledger does; then the
/// current version is stored.
fn check_content_schema(conn: &mut SqliteConnection) -> crate::ledger::Result<()> {
    conn.immediate_transaction::<_, DbError, _>(|tx| {
        let stored: Option<String> = meta::table
            .filter(meta::key.eq("schema"))
            .select(meta::value)
            .first(tx)
            .optional()?;
        if stored.is_some_and(|v| v != SCHEMA_VERSION) {
            diesel::delete(meta::table.filter(meta::key.eq("merkle_root"))).execute(tx)?;
            diesel::delete(files::table).execute(tx)?;
            diesel::delete(chunks::table).execute(tx)?;
            diesel::delete(concepts::table).execute(tx)?;
        }
        diesel::replace_into(meta::table)
            .values((meta::key.eq("schema"), meta::value.eq(SCHEMA_VERSION)))
            .execute(tx)?;
        Ok(())
    })
    .or_backend()
}
