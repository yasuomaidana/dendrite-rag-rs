//! Reads the content-keyed caches of an existing redb ledger, to seed another backend.

use std::path::Path;

use redb::{ReadOnlyDatabase, ReadableDatabase, ReadableTable, TableError};

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
    match txn.open_table(EMBEDDINGS) {
        Ok(table) => {
            for row in table.iter()? {
                let (k, v) = row?;
                embedding(k.value(), v.value())?;
            }
        }
        Err(TableError::TableDoesNotExist(_)) => {}
        Err(e) => return Err(e.into()),
    }
    match txn.open_table(LLM_CACHE) {
        Ok(table) => {
            for row in table.iter()? {
                let (k, v) = row?;
                llm_cache(k.value(), v.value())?;
            }
        }
        Err(TableError::TableDoesNotExist(_)) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
