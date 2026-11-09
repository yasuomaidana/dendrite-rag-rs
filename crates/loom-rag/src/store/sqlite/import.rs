//! The first open of `index.db` next to a `ledger.redb` copies the redb ledger's caches.

use std::cell::RefCell;
use std::path::Path;

use diesel::prelude::*;
use diesel::result::Error as DbError;
use diesel::sqlite::SqliteConnection;

use super::schema::{embeddings, llm_cache, meta};
use super::{SqliteResultExt, split_key};
use crate::ledger::{LedgerError, Result, read_caches};

/// The redb ledger next to `index.db`.
const REDB_FILE: &str = "ledger.redb";
/// The `meta` key that records the copy, so it happens once.
const IMPORT_KEY: &str = "redb_import";

/// Copies the embedding and LLM caches of the `ledger.redb` next to `path` into the ledger,
/// once, so a switch from redb does not re-embed anything. The redb file is only read. A
/// redb file that cannot be read is reported and skipped: its embeddings are recomputed.
pub(super) fn import_redb(conn: &mut SqliteConnection, path: &Path) -> Result<()> {
    let redb = path.with_file_name(REDB_FILE);
    if !redb.is_file() {
        return Ok(());
    }
    let done: Option<String> = meta::table
        .filter(meta::key.eq(IMPORT_KEY))
        .select(meta::value)
        .first(conn)
        .optional()
        .or_backend()?;
    if done.is_some() {
        return Ok(());
    }
    let mut failed: Option<LedgerError> = None;
    let copied = conn.immediate_transaction::<_, DbError, _>(|tx| match copy_caches(tx, &redb) {
        Ok(()) => {
            diesel::replace_into(meta::table)
                .values((meta::key.eq(IMPORT_KEY), meta::value.eq("1")))
                .execute(tx)?;
            Ok(())
        }
        Err(e) => {
            failed = Some(e);
            Err(DbError::RollbackTransaction)
        }
    });
    let Some(e) = failed else {
        return copied.or_backend();
    };
    eprintln!(
        "warning: failed to import the caches of {}: {e}; embeddings will be recomputed",
        redb.display()
    );
    // The copy rolled back with its transaction; record the failure so later opens do not retry it.
    diesel::replace_into(meta::table)
        .values((meta::key.eq(IMPORT_KEY), meta::value.eq("failed")))
        .execute(conn)
        .or_backend()?;
    Ok(())
}

fn copy_caches(conn: &mut SqliteConnection, redb: &Path) -> Result<()> {
    let conn = RefCell::new(conn);
    read_caches(
        redb,
        |key, vec| {
            let (model, hash) = split_key(key);
            let row = (
                embeddings::model.eq(model),
                embeddings::hash.eq(hash),
                embeddings::vec.eq(vec),
            );
            put(
                diesel::insert_or_ignore_into(embeddings::table).values(row),
                &conn,
            )
        },
        |key, value| {
            let (model, hash) = split_key(key);
            let row = (
                llm_cache::model.eq(model),
                llm_cache::prompt_hash.eq(hash),
                llm_cache::value.eq(value),
            );
            put(diesel::insert_or_ignore_into(llm_cache::table).values(row), &conn)
        },
    )
}

/// Runs one insert on the connection the two cache copies share.
fn put(
    stmt: impl RunQueryDsl<SqliteConnection>
    + diesel::query_builder::QueryFragment<diesel::sqlite::Sqlite>
    + diesel::query_builder::QueryId,
    conn: &RefCell<&mut SqliteConnection>,
) -> Result<()> {
    stmt.execute(&mut **conn.borrow_mut()).or_backend()?;
    Ok(())
}
