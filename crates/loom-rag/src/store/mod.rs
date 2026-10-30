//! Storage backend abstraction (`docs/specs/store.md` § Storage backends).

#[cfg(feature = "postgres")]
pub mod edge;
#[cfg(feature = "postgres")]
pub mod pg;
#[cfg(feature = "postgres")]
pub mod push;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::chunk::Chunk;
use crate::config::VectorIndexChoice;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, Ledger};
use crate::lexical::LexicalIndex;
use crate::vector::{FlatIndex, VectorIndex};

pub type Result<T> = crate::engine::Result<T>;

/// A free connection of `pool` if there is one, else the next in turn: concurrent readers do
/// not queue behind each other. `next` is the round-robin counter of the pool.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) fn pick_connection<'a, T>(
    pool: &'a [std::sync::Mutex<T>],
    next: &std::sync::atomic::AtomicUsize,
    what: &str,
) -> std::sync::MutexGuard<'a, T> {
    let start = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    for i in 0..pool.len() {
        if let Ok(c) = pool[(start + i) % pool.len()].try_lock() {
            return c;
        }
    }
    pool[start % pool.len()]
        .lock()
        .unwrap_or_else(|_| panic!("{what} lock"))
}

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
    fn llm_cache(&self, model: &str, prompt_hash: &str) -> crate::ledger::Result<Option<String>>;
    fn concepts(&self, path: &str) -> crate::ledger::Result<Option<String>>;
    fn batch(&self) -> crate::ledger::Result<Box<dyn LedgerWrite + '_>>;
    /// Commits waiting for the shared tier (edge mode); `None` for other backends.
    fn sync_pending(&self) -> Option<usize> {
        None
    }
    /// Sends waiting commits to the shared tier now; returns how many are still waiting.
    fn sync_now(&self) -> crate::ledger::Result<usize> {
        Ok(0)
    }
}

/// An atomic write transaction for the ledger.
pub trait LedgerWrite {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> crate::ledger::Result<()>;
    fn remove_file(&mut self, path: &str) -> crate::ledger::Result<()>;
    fn put_chunk(&mut self, chunk: &Chunk) -> crate::ledger::Result<()>;
    fn remove_chunk(&mut self, id: &str) -> crate::ledger::Result<()>;
    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> crate::ledger::Result<()>;
    fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> crate::ledger::Result<()>;
    fn put_concepts(&mut self, path: &str, json: &str) -> crate::ledger::Result<()>;
    fn remove_concepts(&mut self, path: &str) -> crate::ledger::Result<()>;
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
    fn search(
        &self,
        query: &str,
        limit: usize,
        params: &crate::bm25::Bm25Params,
    ) -> tantivy::Result<Vec<(String, f32)>>;
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

/// Where a project's index lives (`[storage]` in the project settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    /// redb, Tantivy and `vectors.bin` in the project's index directory.
    #[default]
    Local,
    /// The shared PostgreSQL tier (feature `postgres`).
    Remote,
    /// A local cache over the shared tier, with a write journal (DRAG-34).
    Edge,
}

impl StorageMode {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).ok()
    }
}

/// Which backend holds the ledger of a local index (`[storage] ledger`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerChoice {
    /// redb in `ledger.redb`.
    #[default]
    Redb,
    /// SQLite in `index.db` (feature `sqlite`).
    Sqlite,
}

impl LedgerChoice {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).ok()
    }
}

#[derive(Debug, Clone, Default)]
pub struct StorageConfig {
    pub mode: StorageMode,
    /// The repository's name in the shared tier; defaults to its `origin` URL.
    pub repo: Option<String>,
    /// The shared tier URL, set by the caller: credentials never go in a settings file.
    pub pg_url: Option<String>,
    /// The ledger backend of a local index (local and edge modes).
    pub ledger: LedgerChoice,
}

/// The repository name used in the shared tier: the configured one, else the `origin`
/// remote URL, else the project's absolute path.
pub fn repo_name(root: &Path, configured: Option<&str>) -> String {
    if let Some(r) = configured.filter(|r| !r.is_empty()) {
        return r.to_string();
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.display().to_string())
}

impl Store {
    /// Opens the store `cfg.storage` selects for the project at `root`.
    pub fn open(dir: &Path, root: &Path, cfg: &crate::engine::Config) -> Result<Self> {
        let config_err = |msg: String| crate::engine::Error::Other(anyhow::anyhow!(msg));
        match cfg.storage.mode {
            StorageMode::Local => {
                Self::open_local_with(dir, cfg.vector_index, cfg.hnsw_ef_search, cfg.storage.ledger)
            }
            StorageMode::Remote => {
                let Some(url) = cfg.storage.pg_url.as_deref() else {
                    return Err(config_err(
                        "storage mode remote needs the shared tier URL".to_string(),
                    ));
                };
                #[cfg(feature = "postgres")]
                {
                    pg::open(url, &repo_name(root, cfg.storage.repo.as_deref()))
                }
                #[cfg(not(feature = "postgres"))]
                {
                    let _ = (url, root);
                    Err(config_err(
                        "this build was made without the `postgres` feature".to_string(),
                    ))
                }
            }
            StorageMode::Edge => {
                let Some(url) = cfg.storage.pg_url.as_deref() else {
                    return Err(config_err(
                        "storage mode edge needs the shared tier URL".to_string(),
                    ));
                };
                #[cfg(feature = "postgres")]
                {
                    let local =
                        Self::open_local_with(dir, cfg.vector_index, cfg.hnsw_ef_search, cfg.storage.ledger)?;
                    let edge = edge::Edge::new(
                        url,
                        &repo_name(root, cfg.storage.repo.as_deref()),
                        dir.join(edge::JOURNAL_FILE),
                    );
                    Ok(edge::wrap(local, edge))
                }
                #[cfg(not(feature = "postgres"))]
                {
                    let _ = (url, root);
                    Err(config_err(
                        "this build was made without the `postgres` feature".to_string(),
                    ))
                }
            }
        }
    }

    /// Opens the default local storage backend in `dir`: redb ledger, Tantivy lexical index,
    /// and `vectors.bin` flat index (self-healing from cache if corrupt).
    pub fn open_local(dir: &Path) -> Result<Self> {
        Self::open_local_with(dir, VectorIndexChoice::Auto, 64, LedgerChoice::Redb)
    }

    /// Opens the local storage backend in `dir` with the chosen vector index and ledger
    /// backends. The SQLite ledger needs the `sqlite` feature.
    pub fn open_local_with(
        dir: &Path,
        choice: VectorIndexChoice,
        ef_search: usize,
        ledger: LedgerChoice,
    ) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(ignore, "*\n")?;
        }
        let ledger: Box<dyn LedgerStore> = match ledger {
            LedgerChoice::Redb => Box::new(Ledger::open(&dir.join("ledger.redb"))?),
            LedgerChoice::Sqlite => {
                #[cfg(feature = "sqlite")]
                {
                    Box::new(sqlite::SqliteLedger::open(&dir.join(sqlite::INDEX_FILE))?)
                }
                #[cfg(not(feature = "sqlite"))]
                {
                    return Err(crate::engine::Error::Other(anyhow::anyhow!(
                        "ledger \"sqlite\" needs the `sqlite` feature; this build was made without it"
                    )));
                }
            }
        };
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
            // The flat scan: in the DRAG-41 benchmark (768-d, eval/RESULTS.md) it beat HNSW
            // on latency, recall and build time at every size HNSW could be built, and
            // meets the latency target up to about 200k vectors.
            VectorIndexChoice::Auto => {
                let _ = ef_search;
                Box::new(flat)
            }
        };

        Ok(Self {
            ledger,
            lexical: Box::new(lexical),
            vectors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
        let opened = Store::open_local_with(dir.path(), VectorIndexChoice::Hnsw, 64, LedgerChoice::Redb);
        assert!(opened.is_err());
    }

    #[cfg(feature = "hnsw")]
    #[test]
    fn open_local_opens_hnsw_when_feature_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open_local_with(dir.path(), VectorIndexChoice::Hnsw, 64, LedgerChoice::Redb).unwrap();
        assert_eq!(store.vectors.len(), 0);
    }

    #[rstest]
    #[case(" SQLite ", Some(LedgerChoice::Sqlite))]
    #[case("redb", Some(LedgerChoice::Redb))]
    #[case("postgres", None)]
    fn ledger_choice_parses_names(#[case] name: &str, #[case] expected: Option<LedgerChoice>) {
        assert_eq!(LedgerChoice::parse(name), expected);
    }

    #[test]
    fn ledger_choice_default_is_redb() {
        assert_eq!(LedgerChoice::default(), LedgerChoice::Redb);
    }

    #[cfg(not(feature = "sqlite"))]
    #[test]
    fn open_local_rejects_sqlite_without_feature() {
        let dir = tempfile::tempdir().unwrap();
        let opened = Store::open_local_with(dir.path(), VectorIndexChoice::Auto, 64, LedgerChoice::Sqlite);
        let msg = opened.err().expect("sqlite is refused").to_string();
        assert!(msg.contains("`sqlite` feature"), "{msg}");
    }
}
