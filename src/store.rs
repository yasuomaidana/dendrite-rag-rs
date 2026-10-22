//! Storage backend abstraction (`docs/specs/store.md` § Storage backends).

use std::collections::BTreeMap;
use std::path::Path;

use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, Ledger};
use crate::lexical::LexicalIndex;
use crate::settings::VectorIndexChoice;
use crate::vector::{FlatIndex, VectorIndex};

pub type Result<T> = crate::engine::Result<T>;

/// Read-only access to files, chunks, embeddings, metadata, and graph communities.
pub trait LedgerStore: Send + Sync {
    fn files(&self) -> crate::ledger::Result<BTreeMap<String, FileEntry>>;
    fn chunk(&self, id: &str) -> crate::ledger::Result<Option<Chunk>>;
    fn chunks(&self, ids: &[String]) -> crate::ledger::Result<Vec<Chunk>>;
    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> crate::ledger::Result<()>;
    fn chunk_count(&self) -> crate::ledger::Result<u64>;
    fn meta(&self, key: &str) -> crate::ledger::Result<Option<String>>;
    fn embedding(&self, key: &str) -> crate::ledger::Result<Option<Vec<f32>>>;
    fn embeddings(&self, keys: &[String]) -> crate::ledger::Result<Vec<Option<Vec<f32>>>>;
    fn embedding_count(&self) -> crate::ledger::Result<u64>;
    fn community_files(&self) -> crate::ledger::Result<BTreeMap<String, u32>>;
    fn community_of_chunk(&self, chunk: &str) -> crate::ledger::Result<Option<u32>>;
    fn community(&self, id: u32) -> crate::ledger::Result<Option<Community>>;
    fn communities(&self) -> crate::ledger::Result<Vec<Community>>;
    fn batch(&self) -> crate::ledger::Result<Box<dyn LedgerWrite + '_>>;
}

/// An atomic write transaction for the ledger.
pub trait LedgerWrite {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> crate::ledger::Result<()>;
    fn remove_file(&mut self, path: &str) -> crate::ledger::Result<()>;
    fn put_chunk(&mut self, chunk: &Chunk) -> crate::ledger::Result<()>;
    fn remove_chunk(&mut self, id: &str) -> crate::ledger::Result<()>;
    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> crate::ledger::Result<()>;
    fn set_meta(&mut self, key: &str, value: &str) -> crate::ledger::Result<()>;
    fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> crate::ledger::Result<()>;
    fn clear_index(&mut self) -> crate::ledger::Result<()>;
    fn commit(self: Box<Self>) -> crate::ledger::Result<()>;
}

/// Full-text search and document updates for lexical retrieval.
pub trait LexicalStore: Send + Sync {
    fn search(&self, query: &str, limit: usize) -> tantivy::Result<Vec<(String, f32)>>;
    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>>;
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A batch of lexical index updates.
pub trait LexicalWrite {
    fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()>;
    fn remove_file(&mut self, path: &str);
    fn clear(&mut self) -> tantivy::Result<()>;
    fn commit(self: Box<Self>) -> tantivy::Result<()>;
}

/// Combined storage handle holding the ledger, lexical index, and vector index.
pub struct Store {
    pub ledger: Box<dyn LedgerStore>,
    pub lexical: Box<dyn LexicalStore>,
    pub vectors: Box<dyn VectorIndex>,
}

/// The local vector index file inside the index directory.
pub(crate) const VECTORS_FILE: &str = "vectors.bin";
/// The HNSW graph index file inside the index directory.
#[cfg(feature = "hnsw")]
pub(crate) const VECTORS_HNSW_FILE: &str = "vectors.hnsw";

impl Store {
    /// Opens the default local storage backend in `dir`: redb ledger, Tantivy lexical index,
    /// and `vectors.bin` flat index (self-healing from cache if corrupt).
    pub fn open_local(dir: &Path) -> Result<Self> {
        Self::open_local_with(dir, VectorIndexChoice::Auto, 64)
    }

    /// Opens the local storage backend in `dir` with the chosen vector index backend.
    pub fn open_local_with(dir: &Path, choice: VectorIndexChoice, ef_search: usize) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(ignore, "*\n")?;
        }
        let ledger = Ledger::open(&dir.join("ledger.redb"))?;
        let lexical = LexicalIndex::open(&dir.join("tantivy"))?;
        let vectors_path = dir.join(VECTORS_FILE);
        let flat = match FlatIndex::load(&vectors_path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "warning: failed to load vector index at {}: {e}; starting with empty index",
                    vectors_path.display()
                );
                FlatIndex::new().with_path(&vectors_path)
            }
        };

        let vectors: Box<dyn VectorIndex> = match choice {
            VectorIndexChoice::Flat => Box::new(flat),
            VectorIndexChoice::Hnsw => {
                #[cfg(feature = "hnsw")]
                {
                    let hnsw_path = dir.join(VECTORS_HNSW_FILE);
                    let hnsw = crate::vector::HnswIndex::load(&hnsw_path, flat, ef_search)?;
                    Box::new(hnsw)
                }
                #[cfg(not(feature = "hnsw"))]
                {
                    let _ = ef_search;
                    return Err(crate::engine::Error::Other(anyhow::anyhow!(
                        "vector index 'hnsw' requires cargo feature 'hnsw'"
                    )));
                }
            }
            VectorIndexChoice::Auto => {
                #[cfg(feature = "hnsw")]
                if flat.len() > 100_000 {
                    let hnsw_path = dir.join(VECTORS_HNSW_FILE);
                    let hnsw = crate::vector::HnswIndex::load(&hnsw_path, flat, ef_search)?;
                    Box::new(hnsw)
                } else {
                    Box::new(flat)
                }
                #[cfg(not(feature = "hnsw"))]
                {
                    let _ = ef_search;
                    Box::new(flat)
                }
            }
        };

        Ok(Self {
            ledger: Box::new(ledger),
            lexical: Box::new(lexical),
            vectors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_local_creates_store_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_local(dir.path()).unwrap();
        assert_eq!(store.ledger.chunk_count().unwrap(), 0);
        assert_eq!(store.lexical.len(), 0);
        assert_eq!(store.vectors.len(), 0);
    }

    #[cfg(not(feature = "hnsw"))]
    #[test]
    fn open_local_rejects_hnsw_without_feature() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::open_local_with(dir.path(), VectorIndexChoice::Hnsw, 64).is_err());
    }

    #[cfg(feature = "hnsw")]
    #[test]
    fn open_local_opens_hnsw_when_feature_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_local_with(dir.path(), VectorIndexChoice::Hnsw, 64).unwrap();
        assert_eq!(store.vectors.len(), 0);
    }
}
