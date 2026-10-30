//! The first open of `index.db` next to a `ledger.redb` copies the redb ledger's caches.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{SqliteResultExt, split_key};
use crate::ledger::{Result, read_caches};

/// The redb ledger next to `index.db`.
const REDB_FILE: &str = "ledger.redb";
/// The `meta` key that records the copy, so it happens once.
const IMPORT_KEY: &str = "redb_import";

/// Copies the embedding and LLM caches of the `ledger.redb` next to `path` into the ledger,
/// once, so a switch from redb does not re-embed anything. The redb file is only read. A
/// redb file that cannot be read is reported and skipped: its embeddings are recomputed.
pub(super) fn import_redb(conn: &mut Connection, path: &Path) -> Result<()> {
    let redb = path.with_file_name(REDB_FILE);
    if !redb.is_file() {
        return Ok(());
    }
    let done = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [IMPORT_KEY], |r| {
            r.get::<_, String>(0)
        })
        .optional()
        .or_backend()?;
    if done.is_some() {
        return Ok(());
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .or_backend()?;
    match copy_caches(&tx, &redb) {
        Ok(()) => {
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
                [IMPORT_KEY],
            )
            .or_backend()?;
            tx.commit().or_backend()
        }
        Err(e) => {
            eprintln!(
                "warning: failed to import the caches of {}: {e}; embeddings will be recomputed",
                redb.display()
            );
            Ok(())
        }
    }
}

fn copy_caches(conn: &Connection, redb: &Path) -> Result<()> {
    let mut put_embedding = conn
        .prepare_cached("INSERT OR IGNORE INTO embeddings (model, hash, vec) VALUES (?1, ?2, ?3)")
        .or_backend()?;
    let mut put_llm = conn
        .prepare_cached("INSERT OR IGNORE INTO llm_cache (model, prompt_hash, value) VALUES (?1, ?2, ?3)")
        .or_backend()?;
    read_caches(
        redb,
        |key, vec| {
            let (model, hash) = split_key(key);
            put_embedding.execute(params![model, hash, vec]).or_backend()?;
            Ok(())
        },
        |key, value| {
            let (model, prompt_hash) = split_key(key);
            put_llm.execute(params![model, prompt_hash, value]).or_backend()?;
            Ok(())
        },
    )
}
