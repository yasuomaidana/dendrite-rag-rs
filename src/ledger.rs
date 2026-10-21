//! redb ledger: files, chunks, embedding cache and metadata (`docs/specs/store.md`).

use std::collections::BTreeMap;
use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::chunk::Chunk;

const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const CHUNKS: TableDefinition<&str, &[u8]> = TableDefinition::new("chunks");
const EMBEDDINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("embeddings");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");

/// redb page cache size.
const CACHE_BYTES: usize = 8 << 20;

/// Ledger schema version, stored under `meta.schema`.
pub const SCHEMA_VERSION: &str = "1";

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error(transparent)]
    Db(#[from] redb::DatabaseError),
    #[error(transparent)]
    Txn(#[from] redb::TransactionError),
    #[error(transparent)]
    Table(#[from] redb::TableError),
    #[error(transparent)]
    Storage(#[from] redb::StorageError),
    #[error(transparent)]
    Commit(#[from] redb::CommitError),
    #[error("corrupt ledger record: {0}")]
    Decode(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

/// What the ledger remembers about one indexed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// BLAKE3 of the file bytes, hex.
    pub hash: String,
    pub size: u64,
    pub mtime_ns: u128,
    pub chunk_ids: Vec<String>,
}

pub struct Ledger {
    db: Database,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        // redb's default page cache is sized for databases, not a sidecar: bound it so
        // full scans (symbol index, embedding phase) do not inflate resident memory.
        let db = Database::builder().set_cache_size(CACHE_BYTES).create(path)?;
        let txn = db.begin_write()?;
        txn.open_table(FILES)?;
        txn.open_table(CHUNKS)?;
        txn.open_table(EMBEDDINGS)?;
        txn.open_table(META)?.insert("schema", SCHEMA_VERSION)?;
        txn.commit()?;
        Ok(Self { db })
    }

    pub fn files(&self) -> Result<BTreeMap<String, FileEntry>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(FILES)?;
        let mut out = BTreeMap::new();
        for row in table.iter()? {
            let (k, v) = row?;
            out.insert(k.value().to_string(), serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    pub fn chunk(&self, id: &str) -> Result<Option<Chunk>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CHUNKS)?;
        Ok(match table.get(id)? {
            Some(v) => Some(serde_json::from_slice(v.value())?),
            None => None,
        })
    }

    /// Loads several chunks in one read transaction; missing ids are skipped.
    pub fn chunks(&self, ids: &[String]) -> Result<Vec<Chunk>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CHUNKS)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(v) = table.get(id.as_str())? {
                out.push(serde_json::from_slice(v.value())?);
            }
        }
        Ok(out)
    }

    /// Visits every stored chunk.
    pub fn for_each_chunk(&self, mut f: impl FnMut(Chunk)) -> Result<()> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CHUNKS)?;
        for row in table.iter()? {
            let (_, v) = row?;
            f(serde_json::from_slice(v.value())?);
        }
        Ok(())
    }

    pub fn chunk_count(&self) -> Result<u64> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(CHUNKS)?.len()?)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META)?;
        Ok(table.get(key)?.map(|v| v.value().to_string()))
    }

    /// A cached embedding for `key` (`model ‖ content_hash`).
    pub fn embedding(&self, key: &str) -> Result<Option<Vec<f32>>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(EMBEDDINGS)?;
        Ok(table.get(key)?.map(|v| decode_f32(v.value())))
    }

    /// Several cached embeddings in one read transaction, in the order of `keys`.
    pub fn embeddings(&self, keys: &[String]) -> Result<Vec<Option<Vec<f32>>>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(EMBEDDINGS)?;
        keys.iter()
            .map(|k| Ok(table.get(k.as_str())?.map(|v| decode_f32(v.value()))))
            .collect()
    }

    pub fn embedding_count(&self) -> Result<u64> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(EMBEDDINGS)?.len()?)
    }

    /// Starts a write batch; nothing is visible until [`LedgerBatch::commit`].
    pub fn batch(&self) -> Result<LedgerBatch> {
        Ok(LedgerBatch {
            txn: self.db.begin_write()?,
        })
    }
}

/// One atomic set of ledger changes.
pub struct LedgerBatch {
    txn: redb::WriteTransaction,
}

impl LedgerBatch {
    pub fn put_file(&mut self, path: &str, entry: &FileEntry) -> Result<()> {
        self.txn
            .open_table(FILES)?
            .insert(path, serde_json::to_vec(entry)?.as_slice())?;
        Ok(())
    }

    pub fn remove_file(&mut self, path: &str) -> Result<()> {
        self.txn.open_table(FILES)?.remove(path)?;
        Ok(())
    }

    pub fn put_chunk(&mut self, chunk: &Chunk) -> Result<()> {
        self.txn
            .open_table(CHUNKS)?
            .insert(chunk.id.as_str(), serde_json::to_vec(chunk)?.as_slice())?;
        Ok(())
    }

    pub fn remove_chunk(&mut self, id: &str) -> Result<()> {
        self.txn.open_table(CHUNKS)?.remove(id)?;
        Ok(())
    }

    pub fn put_embedding(&mut self, key: &str, vector: &[f32]) -> Result<()> {
        self.txn
            .open_table(EMBEDDINGS)?
            .insert(key, encode_f32(vector).as_slice())?;
        Ok(())
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.txn.open_table(META)?.insert(key, value)?;
        Ok(())
    }

    /// Removes every file and chunk (embeddings are kept: they are keyed by content).
    pub fn clear_index(&mut self) -> Result<()> {
        self.txn.open_table(FILES)?.retain(|_, _| false)?;
        self.txn.open_table(CHUNKS)?.retain(|_, _| false)?;
        Ok(())
    }

    pub fn commit(self) -> Result<()> {
        self.txn.commit()?;
        Ok(())
    }
}

fn encode_f32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn decode_f32(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkOptions, chunk_file};
    use crate::lang::Lang;

    #[test]
    fn round_trips_files_chunks_embeddings_and_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.redb");
        let chunks = chunk_file(
            "a.go",
            Lang::Go,
            "package a\n\nfunc F() {}\n",
            ChunkOptions::default(),
        );
        {
            let ledger = Ledger::open(&path).unwrap();
            let mut b = ledger.batch().unwrap();
            let entry = FileEntry {
                hash: "h".into(),
                size: 3,
                mtime_ns: 7,
                chunk_ids: vec![chunks[0].id.clone()],
            };
            b.put_file("a.go", &entry).unwrap();
            b.put_chunk(&chunks[0]).unwrap();
            b.put_embedding("m:x", &[1.0, -0.5]).unwrap();
            b.set_meta("merkle_root", "abc").unwrap();
            b.commit().unwrap();
        }
        let ledger = Ledger::open(&path).unwrap();
        assert_eq!(ledger.files().unwrap()["a.go"].mtime_ns, 7);
        assert_eq!(ledger.chunk(&chunks[0].id).unwrap().unwrap(), chunks[0]);
        assert_eq!(ledger.embedding("m:x").unwrap().unwrap(), vec![1.0, -0.5]);
        assert_eq!(ledger.meta("merkle_root").unwrap().as_deref(), Some("abc"));
        assert_eq!(ledger.meta("schema").unwrap().as_deref(), Some(SCHEMA_VERSION));

        let mut b = ledger.batch().unwrap();
        b.clear_index().unwrap();
        b.commit().unwrap();
        assert!(ledger.files().unwrap().is_empty());
        assert_eq!(ledger.chunk_count().unwrap(), 0);
        assert_eq!(ledger.embedding_count().unwrap(), 1);
    }
}
