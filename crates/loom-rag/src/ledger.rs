//! redb ledger: files, chunks, embedding cache and metadata (`docs/specs/store.md`).

use std::collections::BTreeMap;
use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::store::{LedgerStore, LedgerWrite};

const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const CHUNKS: TableDefinition<&str, &[u8]> = TableDefinition::new("chunks");
const EMBEDDINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("embeddings");
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");
/// Community id per file and per chunk, and each community's record.
const COMMUNITY_FILES: TableDefinition<&str, u32> = TableDefinition::new("community_files");
const COMMUNITY_CHUNKS: TableDefinition<&str, u32> = TableDefinition::new("community_chunks");
const COMMUNITIES: TableDefinition<u32, &[u8]> = TableDefinition::new("communities");
const LLM_CACHE: TableDefinition<&str, &str> = TableDefinition::new("llm_cache");
const CONCEPTS: TableDefinition<&str, &str> = TableDefinition::new("concepts");

/// redb page cache size.
const CACHE_BYTES: usize = 8 << 20;

/// Composite key for the LLM cache table (`model ‖ prompt_hash`).
pub fn llm_cache_key(model: &str, prompt_hash: &str) -> String {
    format!("{model}\u{0}{prompt_hash}")
}

/// Ledger schema version, stored under `meta.schema`. Version 2 added call references
/// and file imports; an older ledger is cleared on open (embeddings are kept) so the next
/// index run re-chunks every file.
pub const SCHEMA_VERSION: &str = "2";

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
    /// A storage backend other than redb failed (`crate::store`).
    #[error("ledger backend: {0}")]
    Backend(String),
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
    /// Import specifiers as written in the source (see [`crate::chunk::analyze_file`]).
    #[serde(default)]
    pub imports: Vec<String>,
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
        txn.open_table(COMMUNITY_FILES)?;
        txn.open_table(COMMUNITY_CHUNKS)?;
        txn.open_table(COMMUNITIES)?;
        txn.open_table(LLM_CACHE)?;
        txn.open_table(CONCEPTS)?;
        let mut meta = txn.open_table(META)?;
        let stored = meta.get("schema")?.map(|v| v.value().to_string());
        if stored.is_some_and(|v| v != SCHEMA_VERSION) {
            meta.remove("merkle_root")?;
            drop(meta);
            txn.open_table(FILES)?.retain(|_, _| false)?;
            txn.open_table(CHUNKS)?.retain(|_, _| false)?;
            txn.open_table(CONCEPTS)?.retain(|_, _| false)?;
            meta = txn.open_table(META)?;
        }
        meta.insert("schema", SCHEMA_VERSION)?;
        drop(meta);
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

    /// The community id of every file.
    pub fn community_files(&self) -> Result<BTreeMap<String, u32>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(COMMUNITY_FILES)?;
        let mut out = BTreeMap::new();
        for row in table.iter()? {
            let (k, v) = row?;
            out.insert(k.value().to_string(), v.value());
        }
        Ok(out)
    }

    pub fn community_of_chunk(&self, chunk: &str) -> Result<Option<u32>> {
        let txn = self.db.begin_read()?;
        Ok(txn.open_table(COMMUNITY_CHUNKS)?.get(chunk)?.map(|v| v.value()))
    }

    pub fn community(&self, id: u32) -> Result<Option<Community>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(COMMUNITIES)?;
        Ok(match table.get(id)? {
            Some(v) => Some(serde_json::from_slice(v.value())?),
            None => None,
        })
    }

    /// Every community, by id.
    pub fn communities(&self) -> Result<Vec<Community>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(COMMUNITIES)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            out.push(serde_json::from_slice(row?.1.value())?);
        }
        Ok(out)
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

    /// A cached LLM completion for `(model, prompt_hash)`.
    pub fn llm_cache(&self, model: &str, prompt_hash: &str) -> Result<Option<String>> {
        let key = llm_cache_key(model, prompt_hash);
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LLM_CACHE)?;
        Ok(table.get(key.as_str())?.map(|v| v.value().to_string()))
    }

    /// Extracted concepts for a file path (JSON list).
    pub fn concepts(&self, path: &str) -> Result<Option<String>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CONCEPTS)?;
        Ok(table.get(path)?.map(|v| v.value().to_string()))
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

    pub fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> Result<()> {
        let key = llm_cache_key(model, prompt_hash);
        self.txn.open_table(LLM_CACHE)?.insert(key.as_str(), value)?;
        Ok(())
    }

    pub fn put_concepts(&mut self, path: &str, json: &str) -> Result<()> {
        self.txn.open_table(CONCEPTS)?.insert(path, json)?;
        Ok(())
    }

    pub fn remove_concepts(&mut self, path: &str) -> Result<()> {
        self.txn.open_table(CONCEPTS)?.remove(path)?;
        Ok(())
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.txn.open_table(META)?.insert(key, value)?;
        Ok(())
    }

    /// Replaces every community, with the id of each member file and of its chunks.
    pub fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> Result<()> {
        let mut by_file = self.txn.open_table(COMMUNITY_FILES)?;
        let mut by_chunk = self.txn.open_table(COMMUNITY_CHUNKS)?;
        let mut records = self.txn.open_table(COMMUNITIES)?;
        by_file.retain(|_, _| false)?;
        by_chunk.retain(|_, _| false)?;
        records.retain(|_, _| false)?;
        for c in communities {
            records.insert(c.id, serde_json::to_vec(c)?.as_slice())?;
            for f in &c.files {
                by_file.insert(f.as_str(), c.id)?;
                for chunk in files.get(f).map_or(&[][..], |e| e.chunk_ids.as_slice()) {
                    by_chunk.insert(chunk.as_str(), c.id)?;
                }
            }
        }
        Ok(())
    }

    /// Removes every file and chunk (embeddings and LLM cache are kept: they are keyed by content).
    pub fn clear_index(&mut self) -> Result<()> {
        self.txn.open_table(FILES)?.retain(|_, _| false)?;
        self.txn.open_table(CHUNKS)?.retain(|_, _| false)?;
        self.txn.open_table(CONCEPTS)?.retain(|_, _| false)?;
        Ok(())
    }

    pub fn commit(self) -> Result<()> {
        self.txn.commit()?;
        Ok(())
    }
}

impl LedgerStore for Ledger {
    fn files(&self) -> Result<BTreeMap<String, FileEntry>> {
        self.files()
    }

    fn chunk(&self, id: &str) -> Result<Option<Chunk>> {
        self.chunk(id)
    }

    fn chunks(&self, ids: &[String]) -> Result<Vec<Chunk>> {
        self.chunks(ids)
    }

    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> Result<()> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CHUNKS)?;
        for row in table.iter()? {
            let (_, v) = row?;
            f(serde_json::from_slice(v.value())?);
        }
        Ok(())
    }

    fn chunk_count(&self) -> Result<u64> {
        self.chunk_count()
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        self.meta(key)
    }

    fn embedding(&self, key: &str) -> Result<Option<Vec<f32>>> {
        self.embedding(key)
    }

    fn embeddings(&self, keys: &[String]) -> Result<Vec<Option<Vec<f32>>>> {
        self.embeddings(keys)
    }

    fn embedding_count(&self) -> Result<u64> {
        self.embedding_count()
    }

    fn community_files(&self) -> Result<BTreeMap<String, u32>> {
        self.community_files()
    }

    fn community_of_chunk(&self, chunk: &str) -> Result<Option<u32>> {
        self.community_of_chunk(chunk)
    }

    fn community(&self, id: u32) -> Result<Option<Community>> {
        self.community(id)
    }

    fn communities(&self) -> Result<Vec<Community>> {
        self.communities()
    }

    fn llm_cache(&self, model: &str, prompt_hash: &str) -> Result<Option<String>> {
        self.llm_cache(model, prompt_hash)
    }

    fn concepts(&self, path: &str) -> Result<Option<String>> {
        self.concepts(path)
    }

    fn batch(&self) -> Result<Box<dyn LedgerWrite + '_>> {
        Ok(Box::new(self.batch()?))
    }
}

impl LedgerWrite for LedgerBatch {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> Result<()> {
        self.put_file(path, entry)
    }

    fn remove_file(&mut self, path: &str) -> Result<()> {
        self.remove_file(path)
    }

    fn put_chunk(&mut self, chunk: &Chunk) -> Result<()> {
        self.put_chunk(chunk)
    }

    fn remove_chunk(&mut self, id: &str) -> Result<()> {
        self.remove_chunk(id)
    }

    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> Result<()> {
        self.put_embedding(key, vector)
    }

    fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> Result<()> {
        self.put_llm_cache(model, prompt_hash, value)
    }

    fn put_concepts(&mut self, path: &str, json: &str) -> Result<()> {
        self.put_concepts(path, json)
    }

    fn remove_concepts(&mut self, path: &str) -> Result<()> {
        self.remove_concepts(path)
    }

    fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.set_meta(key, value)
    }

    fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> Result<()> {
        self.put_communities(communities, files)
    }

    fn clear_index(&mut self) -> Result<()> {
        self.clear_index()
    }

    fn commit(self: Box<Self>) -> Result<()> {
        (*self).commit()
    }
}

pub(crate) fn encode_f32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(crate) fn decode_f32(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(feature = "sqlite")]
mod import;
#[cfg(feature = "sqlite")]
pub(crate) use import::read_caches;

#[cfg(test)]
mod tests;
