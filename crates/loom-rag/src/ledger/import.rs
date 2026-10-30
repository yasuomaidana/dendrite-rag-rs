//! Reads the content-keyed caches of an existing redb ledger, to seed another backend.

use std::path::Path;

use redb::{ReadOnlyDatabase, ReadableDatabase, ReadableTable, TableDefinition, TableError};

use super::{EMBEDDINGS, LLM_CACHE, Result};

/// Visits every embedding row (`model ‖ 0x00 ‖ content_hash`, little-endian f32 bytes) and
/// every LLM cache row (`model ‖ 0x00 ‖ prompt_hash`, completion) of the ledger at `path`.
/// The file is opened read-only and never modified; a table the file does not have yet is
/// treated as empty.
pub(crate) fn read_caches(
    path: &Path,
    mut embedding: impl FnMut(&str, &[u8]) -> Result<()>,
    mut llm_cache: impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let db = ReadOnlyDatabase::open(path)?;
    let txn = db.begin_read()?;
    visit(&txn, EMBEDDINGS, |k, v| embedding(k, v))?;
    visit(&txn, LLM_CACHE, |k, v| llm_cache(k, v))
}

/// Calls `f` for every row of `table`; a table the file does not have yet is empty.
fn visit<V: redb::Value + 'static>(
    txn: &redb::ReadTransaction,
    table: TableDefinition<&str, V>,
    mut f: impl for<'a> FnMut(&'a str, V::SelfType<'a>) -> Result<()>,
) -> Result<()> {
    match txn.open_table(table) {
        Ok(table) => {
            for row in table.iter()? {
                let (k, v) = row?;
                f(k.value(), v.value())?;
            }
            Ok(())
        }
        Err(TableError::TableDoesNotExist(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
