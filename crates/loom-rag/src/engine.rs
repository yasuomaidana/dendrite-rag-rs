//! The engine: opens a project's index, keeps it current and answers queries.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use embedag::dag::merkle::{MerkleNode, diff};
use lru::LruCache;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chunk::{Chunk, ChunkOptions, analyze_file};
use crate::config::VectorIndexChoice;
use crate::corpus::Corpus;
use crate::embed::{EmbedConfig, Embedder};
use crate::graph::CodeGraph;
use crate::ledger::FileEntry;
use crate::rerank::{RerankConfig, Reranker};
use crate::store::{LedgerStore, LexicalStore, Store};
use crate::symbols::SymbolIndex;
use crate::vector::VectorIndex;
use crate::walk::{SourceFile, WalkOptions, read_text, walk};

mod enrich;
mod index;
mod tier;
mod vectors;

/// The index directory under the project root when `Config::index_dir` is unset.
pub const DEFAULT_INDEX_DIR: &str = ".loom";

/// Query embeddings kept by default (`[index] query_cache`).
pub const DEFAULT_QUERY_CACHE: usize = 256;

/// `(model, query)` → query vector.
type QueryCache = LruCache<(String, String), Vec<f32>>;

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the index lives; defaults to `<root>/.loom`.
    pub index_dir: Option<PathBuf>,
    pub walk: WalkOptions,
    pub chunk: ChunkOptions,
    pub embed: EmbedConfig,
    pub rerank: RerankConfig,
    /// Capacity of the LRU query embedding cache (0 disables).
    pub query_cache: usize,
    /// Leiden resolution for communities: higher gives more, smaller communities.
    pub graph_resolution: f64,
    /// Optional LLM layer for summaries and concepts.
    pub graph_llm: crate::config::GraphLlmSettings,
    /// Vector index backend: auto, flat or hnsw.
    pub vector_index: VectorIndexChoice,
    /// Size of dynamic candidate list for HNSW search.
    pub hnsw_ef_search: usize,
    pub fusion: crate::retrieve::FusionConfig,
    pub storage: crate::store::StorageConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            index_dir: None,
            walk: WalkOptions::default(),
            chunk: ChunkOptions::default(),
            embed: EmbedConfig::default(),
            rerank: RerankConfig::default(),
            query_cache: DEFAULT_QUERY_CACHE,
            graph_resolution: 1.0,
            graph_llm: crate::config::GraphLlmSettings::default(),
            vector_index: VectorIndexChoice::Auto,
            hnsw_ef_search: 128,
            fusion: Default::default(),
            storage: Default::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ledger: {0}")]
    Ledger(#[from] crate::ledger::LedgerError),
    #[error("lexical index: {0}")]
    Lexical(#[from] tantivy::TantivyError),
    #[error("{0}")]
    Embed(#[from] crate::embed::EmbedError),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// The `data.kind` reported over JSON-RPC.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Embed(_) => "embed",
            Error::Ledger(_) | Error::Lexical(_) | Error::Other(_) => "index",
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What `pull` found.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PullReport {
    pub chunks: usize,
    /// Chunks whose embedding is now in the local cache.
    pub cached: usize,
    pub vectors: usize,
}

/// What one `index` run did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexReport {
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_removed: usize,
    pub chunks_added: usize,
    pub chunks_removed: usize,
    /// Chunks embedded in this run (cached embeddings are not counted).
    pub embedded: usize,
    /// Set when embedding failed; the lexical index is still up to date and the
    /// missing vectors are retried on the next run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_error: Option<String>,
    /// Chunks still without a vector because the embedding budget ran out; the next run
    /// continues where this one stopped.
    #[serde(default)]
    pub embed_pending: usize,
    /// Set when LLM enrichment (summaries or concepts) failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_error: Option<String>,
    /// Communities after this run (computed again only when files changed).
    #[serde(default)]
    pub communities: usize,
    /// Time spent opening the engine (stores, embedder and reranker models), reported by
    /// the first run after the engine opened and 0 afterwards.
    #[serde(default)]
    pub open_ms: u64,
    /// Time spent walking the project and hashing changed files.
    #[serde(default)]
    pub walk_ms: u64,
    /// Time spent building the code graph and its communities.
    #[serde(default)]
    pub graph_ms: u64,
    /// Time spent embedding chunks and updating the vector index.
    #[serde(default)]
    pub embed_ms: u64,
    pub duration_ms: u64,
}

/// One retrieved span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub chunk_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub scope: Vec<String>,
    pub score: f32,
    pub content: String,
    /// Other files holding the same text (vendored or copied files), dropped from the
    /// results so the text is read once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<String>,
    /// `content` is the span's outline (doc comment or heading and declaration), not its
    /// text; fetch the text with `chunks.get`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outline: bool,
}

impl Hit {
    pub fn from_chunk(c: &Chunk, score: f32) -> Self {
        Hit {
            chunk_id: c.id.clone(),
            path: c.path.clone(),
            start_line: c.start_line,
            end_line: c.end_line,
            scope: c.scope.clone(),
            score,
            content: c.content.clone(),
            duplicates: Vec::new(),
            outline: false,
        }
    }
}

/// Index statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub files: usize,
    pub chunks: u64,
    pub index_dir: String,
    pub merkle_root: Option<String>,
    /// Unix seconds of the last completed index run.
    pub last_indexed: Option<u64>,
    pub vectors: usize,
    pub embed_model: Option<String>,
    pub communities: usize,
    /// Share of chunks with a vector (`None` without an embedder). Below
    /// [`MIN_EMBED_COVERAGE`] dense retrieval sees too little of the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_coverage: Option<f32>,
    /// Conditions that degrade retrieval, in plain words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Commits waiting for the shared tier (edge mode only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_pending: Option<usize>,
}

/// Embedding coverage under which `status` warns.
pub const MIN_EMBED_COVERAGE: f32 = 0.9;

pub struct Engine {
    pub(crate) root: PathBuf,
    pub(crate) dir: PathBuf,
    pub(crate) cfg: Config,
    pub(crate) ledger: Box<dyn LedgerStore>,
    pub(crate) lexical: Box<dyn LexicalStore>,
    pub(crate) embedder: Option<Box<dyn Embedder>>,
    pub(crate) vectors: Box<dyn VectorIndex>,
    pub(crate) reranker: Option<Box<dyn Reranker>>,
    /// Built lazily from the ledger; cleared whenever an index run changes chunks.
    pub(crate) symbols: RwLock<Option<Arc<SymbolIndex>>>,
    /// The typed code graph, built and cleared like `symbols`.
    pub(crate) graph: RwLock<Option<Arc<CodeGraph>>>,
    /// Query embeddings by `(model, query)`; `None` when `cfg.query_cache` is 0.
    pub(crate) query_cache: Option<Mutex<QueryCache>>,
    pub(crate) llm: Option<crate::llm::Client>,
    /// How long opening took, until the first index run reports it.
    pub(crate) open_ms: u64,
}

impl Engine {
    /// Opens (or creates) the index of the project at `root`, with the embedder from `cfg.embed`.
    pub fn open(root: &Path, cfg: Config) -> Result<Self> {
        let started = Instant::now();
        let embedder = crate::embed::from_config(&cfg.embed)?;
        let mut engine = Self::open_with_embedder(root, cfg, embedder)?;
        engine.open_ms = started.elapsed().as_millis() as u64;
        Ok(engine)
    }

    /// Opens the project with an explicit embedder (`None` disables dense retrieval).
    pub fn open_with_embedder(root: &Path, cfg: Config, embedder: Option<Box<dyn Embedder>>) -> Result<Self> {
        let root = root.canonicalize()?;
        let dir = cfg
            .index_dir
            .clone()
            .unwrap_or_else(|| root.join(DEFAULT_INDEX_DIR));
        let mut store = Store::open(&dir, &root, &cfg)?;
        // Vectors from another model are useless: rebuild them (from the cache when possible).
        if store.ledger.meta("embed_model")?.as_deref() != embedder.as_ref().map(|e| e.model_id()) {
            store.vectors.clear();
        }
        let llm = if cfg.graph_llm.enabled {
            let model = cfg.graph_llm.model.clone().ok_or_else(|| {
                Error::Other(anyhow::anyhow!("graph.llm is enabled but no model is configured"))
            })?;
            Some(crate::llm::Client::from_env(model))
        } else {
            None
        };
        let query_cache = NonZeroUsize::new(cfg.query_cache).map(|n| Mutex::new(LruCache::new(n)));
        Ok(Self {
            root,
            dir,
            ledger: store.ledger,
            lexical: store.lexical,
            reranker: crate::rerank::from_config(&cfg.rerank)?,
            cfg,
            embedder,
            vectors: store.vectors,
            symbols: RwLock::new(None),
            graph: RwLock::new(None),
            query_cache,
            llm,
            open_ms: 0,
        })
    }

    /// Sets an explicit LLM client (e.g. pointing to a local test stub).
    pub fn with_llm(mut self, client: crate::llm::Client) -> Self {
        self.llm = Some(client);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ledger(&self) -> &dyn LedgerStore {
        self.ledger.as_ref()
    }

    /// Whether dense retrieval is available.
    pub fn has_vectors(&self) -> bool {
        self.embedder.is_some() && !self.vectors.is_empty()
    }

    pub fn status(&self) -> Result<Status> {
        let chunks = self.ledger.chunk_count()?;
        let vectors = self.vectors.len();
        let embed_coverage = self.embedder.as_ref().map(|_| {
            if chunks == 0 {
                1.0
            } else {
                (vectors as f32 / chunks as f32).min(1.0)
            }
        });
        let mut warnings = Vec::new();
        if let Some(c) = embed_coverage.filter(|c| *c < MIN_EMBED_COVERAGE) {
            warnings.push(format!(
                "dense retrieval covers {:.0}% of {chunks} chunks; index once without an embedding budget to finish the vectors",
                c * 100.0
            ));
        }
        Ok(Status {
            files: self.ledger.files()?.len(),
            chunks,
            index_dir: self.dir.display().to_string(),
            merkle_root: self.ledger.meta("merkle_root")?,
            last_indexed: self.ledger.meta("last_indexed")?.and_then(|s| s.parse().ok()),
            vectors,
            embed_model: self.embedder.as_ref().map(|e| e.model_id().to_string()),
            communities: self.ledger.communities()?.len(),
            embed_coverage,
            warnings,
            sync_pending: self.ledger.sync_pending(),
        })
    }

    /// Deletes the whole index directory.
    pub fn purge(self) -> Result<()> {
        let dir = self.dir.clone();
        drop(self);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

/// Ledger key of a cached embedding.
pub fn cache_key(model: &str, content_hash: &str) -> String {
    format!("{model}\u{0}{content_hash}")
}

#[cfg(test)]
mod tests;
