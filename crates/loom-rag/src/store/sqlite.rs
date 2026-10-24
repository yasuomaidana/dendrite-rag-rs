//! SQLite storage for the local ledger (`docs/specs/store.md` § Storage backends): files,
//! chunks, embeddings, metadata and communities in one `index.db` file in WAL mode.
//!
//! The tables mirror the redb ledger (`crate::ledger`) and hold the same keys and values,
//! so the two backends behave the same. Reads go through a small pool of connections;
//! writes are buffered and applied in one transaction on commit. The first open of
//! `index.db` next to a `ledger.redb` copies its embedding and LLM caches, so switching
//! backends never re-embeds.

use crate::ledger::LedgerError;

mod import;
mod ledger;
mod schema;

pub use ledger::SqliteLedger;

/// The ledger file inside the index directory.
pub const INDEX_FILE: &str = "index.db";

/// A SQLite failure as a ledger error.
fn backend(e: impl std::fmt::Display) -> LedgerError {
    LedgerError::Backend(format!("sqlite: {e}"))
}

/// `model ‖ 0 ‖ content_hash`, the redb key, split into the two columns of `embeddings`.
fn split_key(key: &str) -> (&str, &str) {
    key.split_once('\0').unwrap_or(("", key))
}

/// Turns a SQLite result into a ledger result.
trait SqliteResultExt<T> {
    fn or_backend(self) -> crate::ledger::Result<T>;
}

impl<T> SqliteResultExt<T> for rusqlite::Result<T> {
    fn or_backend(self) -> crate::ledger::Result<T> {
        self.map_err(backend)
    }
}

#[cfg(test)]
mod tests;
