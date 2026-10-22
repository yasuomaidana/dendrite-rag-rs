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
use crate::embed::{EmbedConfig, Embedder};
use crate::graph::CodeGraph;
use crate::lang::Lang;
use crate::ledger::FileEntry;
use crate::rerank::{RerankConfig, Reranker};
use crate::settings::VectorIndexChoice;
use crate::store::{LedgerStore, LexicalStore, Store};
use crate::symbols::SymbolIndex;
use crate::vector::VectorIndex;
use crate::walk::{SourceFile, WalkOptions, read_text, walk};

/// Characters of a document sent for concept extraction.
const CONCEPT_TEXT_CHARS: usize = 12_000;

/// Documents concepts are extracted from: Markdown and plain prose, not the configuration
/// and data files that are also indexed as text.
fn is_prose(f: &SourceFile) -> bool {
    let ext = std::path::Path::new(&f.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    f.lang == Lang::Markdown || (f.lang == Lang::Text && matches!(ext.as_str(), "txt" | "rst"))
}

/// Query embeddings kept by default (`[index] query_cache`).
pub const DEFAULT_QUERY_CACHE: usize = 256;

/// `(model, query)` → query vector.
type QueryCache = LruCache<(String, String), Vec<f32>>;

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the index lives; defaults to `<root>/.dendrite/rag`.
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
    pub graph_llm: crate::settings::GraphLlmSettings,
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
            graph_llm: crate::settings::GraphLlmSettings::default(),
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
    /// Time spent building the code graph and its communities.
    #[serde(default)]
    pub graph_ms: u64,
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
    /// Commits waiting for the shared tier (edge mode only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_pending: Option<usize>,
}

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
}

impl Engine {
    /// Opens (or creates) the index of the project at `root`, with the embedder from `cfg.embed`.
    pub fn open(root: &Path, cfg: Config) -> Result<Self> {
        let embedder = crate::embed::from_config(&cfg.embed)?;
        Self::open_with_embedder(root, cfg, embedder)
    }

    /// Opens the project with an explicit embedder (`None` disables dense retrieval).
    pub fn open_with_embedder(root: &Path, cfg: Config, embedder: Option<Box<dyn Embedder>>) -> Result<Self> {
        let root = root.canonicalize()?;
        let dir = cfg
            .index_dir
            .clone()
            .unwrap_or_else(|| root.join(".dendrite").join("rag"));
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

    /// Brings the index up to date. Unchanged files are skipped by `(size, mtime)` and then
    /// by content hash; the changed set comes from a Merkle diff of `path → hash`.
    /// `full` discards the index and rebuilds it.
    pub fn index(&mut self, full: bool) -> Result<IndexReport> {
        self.index_with(full, None)
    }

    /// Like [`Engine::index`], but stops the embedding phase once `embed_budget` has elapsed
    /// since the call started. The lexical index is always completed; chunks left without
    /// vectors are counted in `embed_pending` and embedded by later runs.
    pub fn index_with(
        &mut self,
        full: bool,
        embed_budget: Option<std::time::Duration>,
    ) -> Result<IndexReport> {
        let started = Instant::now();
        let deadline = embed_budget.map(|b| started + b);
        let files = walk(&self.root, &self.cfg.walk)?;
        let old = if full {
            BTreeMap::new()
        } else {
            self.ledger.files()?
        };

        // Hash every file, reusing the stored hash when size and mtime are unchanged.
        let scanned: Vec<(SourceFile, String)> = files
            .into_par_iter()
            .filter_map(|f| {
                if let Some(e) = old
                    .get(&f.path)
                    .filter(|e| e.size == f.size && e.mtime_ns == f.mtime_ns)
                {
                    return Some((f, e.hash.clone()));
                }
                let bytes = std::fs::read(&f.abs).ok()?;
                let hash = blake3::hash(&bytes).to_hex().to_string();
                Some((f, hash))
            })
            .collect();

        let old_tree = merkle(old.iter().map(|(p, e)| (p.as_str(), e.hash.as_str())));
        let new_tree = merkle(scanned.iter().map(|(f, h)| (f.path.as_str(), h.as_str())));
        let new_paths: BTreeSet<&str> = scanned.iter().map(|(f, _)| f.path.as_str()).collect();
        let delta = diff(&old_tree, &new_tree);

        let mut removed: BTreeSet<String> = BTreeSet::new();
        let mut added: BTreeSet<String> = BTreeSet::new();
        for p in delta.removed.iter().chain(&delta.changed) {
            removed.extend(expand(p, old.keys().map(String::as_str)));
        }
        for p in delta.added.iter().chain(&delta.changed) {
            added.extend(expand(p, new_paths.iter().copied()));
        }

        // Chunk the added and modified files.
        let opts = self.cfg.chunk;
        let fresh: Vec<(&SourceFile, &String, Vec<Chunk>, Vec<String>)> = scanned
            .par_iter()
            .filter(|(f, _)| added.contains(&f.path))
            .map(|(f, h)| {
                let (chunks, imports) = read_text(&f.abs)
                    .map_or_else(Default::default, |src| analyze_file(&f.path, f.lang, &src, opts));
                (f, h, chunks, imports)
            })
            .collect();

        let mut report = IndexReport {
            files_scanned: scanned.len(),
            files_changed: added.len(),
            files_removed: removed.iter().filter(|p| !new_paths.contains(p.as_str())).count(),
            ..IndexReport::default()
        };

        // Lexical index first: replaying a half-finished run is idempotent, because files
        // are removed by path before their chunks are added again.
        let mut lw = self.lexical.writer()?;
        // An empty file table (first run, or a ledger cleared by a schema change) must not
        // keep the lexical documents of an earlier run: they would be duplicated.
        if full || old.is_empty() {
            lw.clear()?;
        }
        for p in &removed {
            lw.remove_file(p);
        }
        for (_, _, chunks, _) in &fresh {
            for c in chunks {
                lw.add(c)?;
            }
        }
        lw.commit()?;

        let mut batch = self.ledger.batch()?;
        if full {
            batch.clear_index()?;
        }
        for p in &removed {
            if let Some(e) = old.get(p) {
                for id in &e.chunk_ids {
                    batch.remove_chunk(id)?;
                    report.chunks_removed += 1;
                }
            }
            if !new_paths.contains(p.as_str()) {
                batch.remove_file(p)?;
                batch.remove_concepts(p)?;
            }
        }
        for (f, h, chunks, imports) in &fresh {
            for c in chunks {
                batch.put_chunk(c)?;
            }
            report.chunks_added += chunks.len();
            let entry = FileEntry {
                hash: (*h).clone(),
                size: f.size,
                mtime_ns: f.mtime_ns,
                chunk_ids: chunks.iter().map(|c| c.id.clone()).collect(),
                imports: imports.clone(),
            };
            batch.put_file(&f.path, &entry)?;
        }
        // Unchanged content with a new mtime: refresh the entry so the next run skips it.
        for (f, h) in &scanned {
            if added.contains(&f.path) {
                continue;
            }
            if let Some(e) = old
                .get(&f.path)
                .filter(|e| e.size != f.size || e.mtime_ns != f.mtime_ns)
            {
                let entry = FileEntry {
                    hash: h.clone(),
                    size: f.size,
                    mtime_ns: f.mtime_ns,
                    ..e.clone()
                };
                batch.put_file(&f.path, &entry)?;
            }
        }
        batch.set_meta("merkle_root", &hex(&new_tree.hash))?;
        batch.set_meta("last_indexed", &now_secs().to_string())?;
        batch.commit()?;

        let changed = full || report.files_changed > 0 || report.files_removed > 0;
        if changed {
            *self.symbols.write().expect("symbols lock") = None;
            *self.graph.write().expect("graph lock") = None;
        }
        // Communities follow the graph; a ledger from before communities gets them now.
        let mut communities = self.ledger.communities()?.len();
        if changed || (communities == 0 && !scanned.is_empty()) {
            let t = Instant::now();
            communities = self.update_communities(&mut report)?;
            report.graph_ms = t.elapsed().as_millis() as u64;
        }
        report.communities = communities;
        self.enrich_concepts(&scanned, &added, &mut report)?;
        self.sync_vectors(changed, deadline, &mut report)?;
        report.duration_ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }

    /// Makes the vector index hold exactly one vector per stored chunk, embedding only
    /// chunks whose `(model, content_hash)` is not cached. Embedding failures are reported,
    /// not raised, so the lexical index stays usable.
    fn sync_vectors(
        &mut self,
        changed: bool,
        deadline: Option<Instant>,
        report: &mut IndexReport,
    ) -> Result<()> {
        let Some(embedder) = &self.embedder else {
            if !self.vectors.is_empty() {
                self.vectors.clear();
                self.vectors.persist()?;
            }
            return Ok(());
        };
        let model = embedder.model_id().to_string();
        let chunk_count = self.ledger.chunk_count()? as usize;
        let same_model = self.ledger.meta("embed_model")?.as_deref() == Some(model.as_str());
        if !changed && same_model && self.vectors.len() == chunk_count {
            // Equal counts can still hide a stale file: compare ids too. The file table
            // lists every chunk id without loading chunk texts.
            let files = self.ledger.files()?;
            if files
                .values()
                .flat_map(|f| &f.chunk_ids)
                .all(|id| self.vectors.contains(id))
            {
                return Ok(());
            }
        }

        // Only ids and content hashes are kept in memory; texts are loaded per group.
        let mut live: BTreeMap<String, String> = BTreeMap::new();
        self.ledger.for_each_chunk(&mut |c| {
            live.insert(c.id, c.content_hash);
        })?;
        let stale: Vec<String> = self
            .vectors
            .ids()
            .filter(|id| !live.contains_key(*id))
            .map(str::to_string)
            .collect();
        for id in &stale {
            self.vectors.remove(id);
        }

        let mut todo: Vec<String> = Vec::new();
        let missing: Vec<(&String, &String)> =
            live.iter().filter(|(id, _)| !self.vectors.contains(id)).collect();
        for (id, hash) in missing {
            match self.ledger.embedding(&cache_key(&model, hash))? {
                Some(v) => self.vectors.upsert(id, hash, &v),
                None => todo.push(id.clone()),
            }
        }

        // Embed in groups, committing each group's cache entries so progress survives
        // interruptions.
        // Under a budget, small groups keep the overshoot to a fraction of a second.
        let group = if deadline.is_some() {
            EMBED_GROUP_BUDGETED
        } else {
            EMBED_GROUP
        };
        for (n, ids) in todo.chunks(group).enumerate() {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.embed_pending = todo.len() - n * group;
                break;
            }
            let group = self.ledger.chunks(ids)?;
            let texts: Vec<String> = group.iter().map(|c| c.indexed_text()).collect();
            let vectors = match embedder.embed_documents(&texts) {
                Ok(v) => v,
                Err(e) => {
                    report.embed_error = Some(e.to_string());
                    break;
                }
            };
            let mut batch = self.ledger.batch()?;
            for (c, v) in group.iter().zip(&vectors) {
                batch.put_embedding(&cache_key(&model, &c.content_hash), v)?;
                self.vectors.upsert(&c.id, &c.content_hash, v);
            }
            batch.commit()?;
            report.embedded += group.len();
        }

        let mut batch = self.ledger.batch()?;
        batch.set_meta("embed_model", &model)?;
        batch.commit()?;
        self.vectors.persist()?;
        Ok(())
    }

    /// Dense-only search; empty when no embedder is configured.
    pub fn search_dense(&self, query: &str, top_k: usize) -> Result<Vec<Hit>> {
        let ranked = self.dense_ranking(query, top_k)?;
        self.hits(&ranked)
    }

    /// Embeds a query with the configured embedder (`None` without one).
    pub fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
        let Some(embedder) = &self.embedder else {
            return Ok(None);
        };
        Ok(Some(self.cached_embed_query(embedder.as_ref(), query)?))
    }

    /// Nearest chunks to an already-embedded query. The int8 scan shortlists
    /// `RESCORE_FACTOR × top_k` rows, which are rescored with their exact `f32` vectors
    /// from the embedding cache.
    pub fn search_vector(&self, query: &[f32], top_k: usize) -> Result<Vec<(String, f32)>> {
        let mut shortlist = self
            .vectors
            .search(query, (top_k * RESCORE_FACTOR).max(RESCORE_MIN));
        if let Some(model) = self.embedder.as_ref().map(|e| e.model_id()) {
            let keys: Vec<String> = shortlist
                .iter()
                .map(|(id, _)| cache_key(model, self.vectors.tag(id).unwrap_or_default()))
                .collect();
            for ((_, score), exact) in shortlist.iter_mut().zip(self.ledger.embeddings(&keys)?) {
                if let Some(v) = exact {
                    *score = v.iter().zip(query).map(|(a, b)| a * b).sum();
                }
            }
            shortlist.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        shortlist.truncate(top_k);
        Ok(shortlist)
    }

    /// Embeds a query, reusing the vector of an identical earlier query with the same model.
    fn cached_embed_query(&self, embedder: &dyn Embedder, query: &str) -> Result<Vec<f32>> {
        let Some(cache) = &self.query_cache else {
            return Ok(embedder.embed_query(query)?);
        };
        let key = (embedder.model_id().to_string(), query.to_string());
        if let Some(v) = cache.lock().expect("query cache lock").get(&key) {
            return Ok(v.clone());
        }
        let v = embedder.embed_query(query)?;
        cache.lock().expect("query cache lock").put(key, v.clone());
        Ok(v)
    }

    /// `(chunk_id, cosine)` for the query, best first.
    pub fn dense_ranking(&self, query: &str, top_k: usize) -> Result<Vec<(String, f32)>> {
        let Some(embedder) = &self.embedder else {
            return Ok(Vec::new());
        };
        if self.vectors.is_empty() {
            return Ok(Vec::new());
        }
        let q = self.cached_embed_query(embedder.as_ref(), query)?;
        self.search_vector(&q, top_k)
    }

    /// BM25-only search.
    pub fn search_lexical(&self, query: &str, top_k: usize) -> Result<Vec<Hit>> {
        let ranked = self.lexical.search(query, top_k)?;
        self.hits(&ranked)
    }

    /// Resolves ranked chunk ids into hits, keeping the order.
    pub fn hits(&self, ranked: &[(String, f32)]) -> Result<Vec<Hit>> {
        let ids: Vec<String> = ranked.iter().map(|(id, _)| id.clone()).collect();
        let chunks: BTreeMap<String, Chunk> = self
            .ledger
            .chunks(&ids)?
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        Ok(ranked
            .iter()
            .filter_map(|(id, s)| chunks.get(id).map(|c| Hit::from_chunk(c, *s)))
            .collect())
    }

    pub fn status(&self) -> Result<Status> {
        Ok(Status {
            files: self.ledger.files()?.len(),
            chunks: self.ledger.chunk_count()?,
            index_dir: self.dir.display().to_string(),
            merkle_root: self.ledger.meta("merkle_root")?,
            last_indexed: self.ledger.meta("last_indexed")?.and_then(|s| s.parse().ok()),
            vectors: self.vectors.len(),
            embed_model: self.embedder.as_ref().map(|e| e.model_id().to_string()),
            communities: self.ledger.communities()?.len(),
            sync_pending: self.ledger.sync_pending(),
        })
    }

    /// Sends commits journaled while the shared tier was unreachable (edge mode); returns
    /// how many are still waiting.
    pub fn sync(&self) -> Result<usize> {
        Ok(self.ledger.sync_now()?)
    }

    /// Uploads this project's index to the shared tier, records the current commit's chunks
    /// and compacts the tier (`docs/specs/store.md` § Push and compaction). With `dry_run`
    /// the same work runs in a transaction that is rolled back.
    #[cfg(feature = "postgres")]
    pub fn push(&self, dry_run: bool) -> Result<crate::store::push::PushReport> {
        use crate::store::push::{GitState, LocalIndex};
        let other = |e: String| Error::Other(anyhow::anyhow!(e));
        let url = self
            .cfg
            .storage
            .pg_url
            .as_deref()
            .ok_or_else(|| other("push needs DENDRITE_RAG_PG_URL".to_string()))?;
        let git = GitState::read(&self.root).map_err(other)?;
        let repo = crate::store::repo_name(&self.root, self.cfg.storage.repo.as_deref());
        let mut chunks = Vec::new();
        self.ledger.for_each_chunk(&mut |c| chunks.push(c))?;
        let mut embeddings = Vec::new();
        if let Some(e) = &self.embedder {
            let model = e.model_id().to_string();
            let mut hashes: Vec<String> = chunks.iter().map(|c| c.content_hash.clone()).collect();
            hashes.sort();
            hashes.dedup();
            let keys: Vec<String> = hashes.iter().map(|h| cache_key(&model, h)).collect();
            for (h, v) in hashes.iter().zip(self.ledger.embeddings(&keys)?) {
                if let Some(v) = v {
                    embeddings.push((model.clone(), h.clone(), v));
                }
            }
        }
        let vectors = self
            .vectors
            .ids()
            .filter_map(|id| Some((id.to_string(), self.vectors.tag(id)?.to_string())))
            .collect();
        let mut meta = Vec::new();
        for key in ["merkle_root", "last_indexed", "embed_model"] {
            if let Some(v) = self.ledger.meta(key)? {
                meta.push((key.to_string(), v));
            }
        }
        let local = LocalIndex {
            files: self.ledger.files()?,
            chunks,
            embeddings,
            vectors,
            communities: self.ledger.communities()?,
            meta,
        };
        let pg = crate::store::pg::Pg::connect(url, &repo).map_err(other)?;
        crate::store::push::push(&pg, local, &git, dry_run).map_err(|e| other(format!("postgres: {e}")))
    }

    /// Fetches every embedding the current chunks need that is missing locally (edge mode
    /// reads them from the shared tier), then fills the vector index, so the project can be
    /// searched offline. Returns how many chunks have a vector.
    pub fn pull(&mut self) -> Result<PullReport> {
        let Some(embedder) = &self.embedder else {
            return Ok(PullReport::default());
        };
        let model = embedder.model_id().to_string();
        let mut keys: Vec<String> = Vec::new();
        self.ledger
            .for_each_chunk(&mut |c| keys.push(cache_key(&model, &c.content_hash)))?;
        let mut available = 0;
        for group in keys.chunks(512) {
            available += self
                .ledger
                .embeddings(group)?
                .iter()
                .filter(|v| v.is_some())
                .count();
        }
        let mut report = IndexReport::default();
        self.sync_vectors(true, Some(Instant::now()), &mut report)?;
        Ok(PullReport {
            chunks: keys.len(),
            cached: available,
            vectors: self.vectors.len(),
        })
    }

    /// Recomputes the communities of the code graph and stores them; returns their count.
    /// Each community of two or more files gets a summary chunk, replacing the last run's:
    /// lexical index first, then the ledger, like an index run; vectors follow in
    /// `sync_vectors`.
    fn update_communities(&self, report: &mut IndexReport) -> Result<usize> {
        use crate::graph::summary;
        let graph = self.code_graph()?;
        let previous = self.ledger.community_files()?;
        let old_summaries: Vec<String> = self
            .ledger
            .communities()?
            .into_iter()
            .filter_map(|c| c.summary_chunk)
            .collect();
        let mut communities = crate::graph::communities::detect(&graph, &previous, self.cfg.graph_resolution);

        let mut summaries: Vec<Chunk> = Vec::new();
        let mut new_cache: Vec<(String, String, String)> = Vec::new();

        let use_llm_summaries =
            self.cfg.graph_llm.enabled && self.cfg.graph_llm.summaries && self.llm.is_some();

        for c in &mut communities {
            let Some(mut chunk) = summary::summary_chunk(c) else {
                continue;
            };

            if use_llm_summaries {
                let llm = self.llm.as_ref().unwrap();
                let model = llm.model();
                let mut top_symbol_lines = Vec::new();
                for sym in &c.top_symbols {
                    if let Ok(Some(sc)) = self.ledger.chunk(&sym.chunk)
                        && let Some(first_line) = sc.content.lines().find(|l| !l.trim().is_empty())
                    {
                        top_symbol_lines.push(format!("{}: {}", sym.name, first_line.trim()));
                    }
                }
                let prompt = crate::llm::build_community_summary_prompt(&chunk.content, &top_symbol_lines);
                let prompt_hash = blake3::hash(prompt.as_bytes()).to_hex().to_string();

                // After one failure the gateway is not asked again in this run: a down
                // gateway must not cost one timeout per community.
                let desc_res = match self.ledger.llm_cache(model, &prompt_hash)? {
                    Some(cached) => Ok(cached),
                    None if report.llm_error.is_some() => Err(()),
                    None => match llm.complete(&prompt) {
                        Ok(desc) => {
                            new_cache.push((model.to_string(), prompt_hash, desc.clone()));
                            Ok(desc)
                        }
                        Err(e) => {
                            report.llm_error = Some(e.to_string());
                            Err(())
                        }
                    },
                };

                if let Ok(desc) = desc_res {
                    summary::append_llm_description(&mut chunk, &desc);
                }
            }

            c.summary_chunk = Some(chunk.id.clone());
            summaries.push(chunk);
        }

        let mut lw = self.lexical.writer()?;
        for id in &old_summaries {
            if let Some(cid) = summary::community_of(id) {
                lw.remove_file(&format!("{}{cid}", summary::SUMMARY_PREFIX));
            }
        }
        for c in &summaries {
            lw.remove_file(&c.path);
            lw.add(c)?;
        }
        lw.commit()?;

        let mut batch = self.ledger.batch()?;
        for (model, prompt_hash, desc) in new_cache {
            batch.put_llm_cache(&model, &prompt_hash, &desc)?;
        }
        for id in &old_summaries {
            batch.remove_chunk(id)?;
        }
        for c in &summaries {
            batch.put_chunk(c)?;
        }
        batch.put_communities(&communities, &self.ledger.files()?)?;
        batch.commit()?;
        Ok(communities.len())
    }

    /// Extracts up to 8 concepts per Markdown/text file when `concepts` is enabled.
    fn enrich_concepts(
        &self,
        scanned: &[(SourceFile, String)],
        added: &BTreeSet<String>,
        report: &mut IndexReport,
    ) -> Result<()> {
        if !self.cfg.graph_llm.enabled || !self.cfg.graph_llm.concepts {
            return Ok(());
        }
        let Some(llm) = &self.llm else {
            return Ok(());
        };
        let model = llm.model();

        let mut batch = self.ledger.batch()?;
        for (f, _) in scanned {
            if !is_prose(f) {
                continue;
            }
            let is_new_or_modified = added.contains(&f.path);
            let has_concepts = self.ledger.concepts(&f.path)?.is_some();
            if !is_new_or_modified && has_concepts {
                continue;
            }
            let Some(text) = read_text(&f.abs) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }

            let text: String = text.chars().take(CONCEPT_TEXT_CHARS).collect();
            let prompt = crate::llm::build_concepts_prompt(&text);
            let prompt_hash = blake3::hash(prompt.as_bytes()).to_hex().to_string();

            let raw_res = match self.ledger.llm_cache(model, &prompt_hash)? {
                Some(cached) => Ok(cached),
                None if report.llm_error.is_some() => Err(()),
                None => match llm.complete_json(&prompt) {
                    Ok(resp) => {
                        batch.put_llm_cache(model, &prompt_hash, &resp)?;
                        Ok(resp)
                    }
                    Err(e) => {
                        report.llm_error = Some(e.to_string());
                        Err(())
                    }
                },
            };

            if let Ok(resp) = raw_res
                && let Some(concepts) = crate::llm::parse_concepts(&resp)
            {
                let json = serde_json::to_string(&concepts).map_err(crate::ledger::LedgerError::from)?;
                batch.put_concepts(&f.path, &json)?;
            }
        }
        batch.commit()?;
        Ok(())
    }

    /// Deletes the whole index directory.
    pub fn purge(self) -> Result<()> {
        let dir = self.dir.clone();
        drop(self);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

/// The int8 shortlist is this many times `top_k` (at least `RESCORE_MIN`) before exact
/// rescoring.
const RESCORE_FACTOR: usize = 4;
const RESCORE_MIN: usize = 40;
/// Chunks per embedding call group (and per cache commit).
const EMBED_GROUP: usize = 64;
/// Group size when the embedding phase runs under a time budget.
const EMBED_GROUP_BUDGETED: usize = 8;

/// Ledger key of a cached embedding.
pub fn cache_key(model: &str, content_hash: &str) -> String {
    format!("{model}\u{0}{content_hash}")
}

fn merkle<'a>(entries: impl Iterator<Item = (&'a str, &'a str)>) -> MerkleNode {
    MerkleNode::from_paths("", entries.map(|(p, h)| (p.to_string(), h.as_bytes().to_vec())))
}

/// The files a delta path stands for: the path itself, or every file under it when the
/// diff reports a whole directory.
fn expand<'a>(delta_path: &str, files: impl Iterator<Item = &'a str>) -> Vec<String> {
    let p = delta_path.trim_start_matches('/');
    let prefix = format!("{p}/");
    files
        .filter(|f| *f == p || p.is_empty() || f.starts_with(&prefix))
        .map(str::to_string)
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::VECTORS_FILE;
    use crate::testutil::{HashEmbedder, project};
    use crate::vector::FlatIndex;
    use std::fs;

    fn open(root: &Path) -> Engine {
        Engine::open_with_embedder(root, Config::default(), None).unwrap()
    }

    #[test]
    fn dense_search_uses_cached_embeddings() {
        let dir = project();
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        let r = e.index(false).unwrap();
        assert_eq!(r.embedded, r.chunks_added);
        assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);
        let hits = e.search_dense("LocateBinary sidecar", 1).unwrap();
        assert_eq!(hits[0].path, "pkg/sidecar/sidecar.go");

        // A full rebuild re-chunks everything but embeds nothing new.
        let r = e.index(true).unwrap();
        assert_eq!(r.embedded, 0);
        assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);

        // Reopening loads the persisted vectors.
        drop(e);
        let e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        assert!(e.has_vectors());
        assert_eq!(
            e.search_dense("IndexProject", 1).unwrap()[0].path,
            "pkg/rag/indexer.go"
        );
    }

    #[test]
    fn embedding_budget_defers_work_to_later_runs() {
        let dir = project();
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        let r = e.index_with(false, Some(std::time::Duration::ZERO)).unwrap();
        assert_eq!(r.embedded, 0);
        assert_eq!(r.embed_pending as u64, e.status().unwrap().chunks);
        assert_eq!(
            e.search_lexical("LocateBinary", 1).unwrap().len(),
            1,
            "lexical index is complete"
        );

        let r = e.index(false).unwrap();
        assert_eq!((r.files_changed, r.embed_pending), (0, 0));
        assert_eq!(r.embedded as u64, e.status().unwrap().chunks);
    }

    #[test]
    fn embedding_failures_are_reported_and_retried() {
        use std::sync::atomic::Ordering;
        let dir = project();
        let failing = HashEmbedder::new();
        failing.fail.store(true, Ordering::SeqCst);
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(failing))).unwrap();
        let r = e.index(false).unwrap();
        assert!(r.embed_error.as_deref().unwrap().contains("gateway down"));
        assert_eq!(e.search_lexical("LocateBinary", 1).unwrap().len(), 1);
        drop(e);

        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        let r = e.index(false).unwrap();
        assert!(r.embed_error.is_none());
        assert_eq!(r.files_changed, 0);
        assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);
    }

    #[test]
    fn indexes_then_skips_unchanged_files() {
        let dir = project();
        let mut e = open(dir.path());
        let r = e.index(false).unwrap();
        assert_eq!((r.files_scanned, r.files_changed, r.files_removed), (3, 3, 0));
        assert!(r.chunks_added >= 3);
        let r = e.index(false).unwrap();
        assert_eq!((r.files_changed, r.chunks_added, r.chunks_removed), (0, 0, 0));
        assert_eq!(
            e.search_lexical("locate binary", 3).unwrap()[0].path,
            "pkg/sidecar/sidecar.go"
        );
    }

    #[test]
    fn edits_deletes_and_renames_are_picked_up() {
        let dir = project();
        let root = dir.path();
        let mut e = open(root);
        e.index(false).unwrap();

        fs::write(
            root.join("pkg/rag/indexer.go"),
            "package rag\n\nfunc RetrieveChunks() {}\n",
        )
        .unwrap();
        let r = e.index(false).unwrap();
        assert_eq!(r.files_changed, 1);
        assert!(e.search_lexical("IndexProject", 3).unwrap().is_empty());
        assert_eq!(
            e.search_lexical("retrieve chunks", 3).unwrap()[0].path,
            "pkg/rag/indexer.go"
        );

        fs::rename(root.join("pkg/sidecar"), root.join("pkg/helper")).unwrap();
        let r = e.index(false).unwrap();
        assert_eq!((r.files_changed, r.files_removed), (1, 1));
        let hits = e.search_lexical("LocateBinary", 3).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "pkg/helper/sidecar.go");

        fs::remove_file(root.join("README.md")).unwrap();
        let r = e.index(false).unwrap();
        assert_eq!(r.files_removed, 1);
        assert_eq!(e.status().unwrap().files, 2);
    }

    #[test]
    fn touching_without_changes_reindexes_nothing() {
        let dir = project();
        let root = dir.path();
        let mut e = open(root);
        e.index(false).unwrap();
        let p = root.join("README.md");
        let content = fs::read(&p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, content).unwrap();
        let r = e.index(false).unwrap();
        assert_eq!(r.files_changed, 0);
    }

    #[test]
    fn full_rebuild_status_and_purge() {
        let dir = project();
        let mut e = open(dir.path());
        e.index(false).unwrap();
        let before = e.status().unwrap();
        let r = e.index(true).unwrap();
        assert_eq!(r.files_changed, 3);
        let after = e.status().unwrap();
        assert_eq!(before.chunks, after.chunks);
        assert_eq!(before.merkle_root, after.merkle_root);
        assert!(after.last_indexed.is_some());
        let index_dir = PathBuf::from(&after.index_dir);
        assert!(index_dir.join(".gitignore").exists());
        e.purge().unwrap();
        assert!(!index_dir.exists());
    }

    struct SharedEmbedder(Arc<HashEmbedder>);

    impl Embedder for SharedEmbedder {
        fn model_id(&self) -> &str {
            self.0.model_id()
        }

        fn embed_documents(&self, texts: &[String]) -> crate::embed::Result<Vec<Vec<f32>>> {
            self.0.embed_documents(texts)
        }

        fn embed_query(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
            self.0.embed_query(text)
        }
    }

    #[test]
    fn truncated_or_corrupt_vectors_heals_from_cache() {
        use std::sync::atomic::Ordering;
        let dir = project();
        let embedder = Arc::new(HashEmbedder::new());
        let mut e = Engine::open_with_embedder(
            dir.path(),
            Config::default(),
            Some(Box::new(SharedEmbedder(embedder.clone()))),
        )
        .unwrap();
        let r = e.index(false).unwrap();
        assert_eq!(r.embedded, r.chunks_added);
        assert!(e.status().unwrap().vectors > 0);
        let hits_before = e.search_dense("LocateBinary sidecar", 1).unwrap();
        assert_eq!(hits_before[0].path, "pkg/sidecar/sidecar.go");
        let initial_calls = embedder.calls.load(Ordering::SeqCst);
        assert!(initial_calls > 0);
        drop(e);

        // Corrupt vectors.bin by truncating it
        let vectors_path = dir.path().join(".dendrite/rag").join(VECTORS_FILE);
        assert!(vectors_path.exists());
        fs::write(&vectors_path, b"DRV3-truncated").unwrap();

        // Engine::open succeeds on corrupt vectors.bin
        let embedder = Arc::new(HashEmbedder::new());
        let mut e = Engine::open_with_embedder(
            dir.path(),
            Config::default(),
            Some(Box::new(SharedEmbedder(embedder.clone()))),
        )
        .unwrap();
        assert_eq!(e.status().unwrap().vectors, 0);

        // After index, dense search works with zero embed calls
        let r = e.index(false).unwrap();
        assert_eq!(r.embedded, 0);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);

        let hits_after = e.search_dense("LocateBinary sidecar", 1).unwrap();
        assert_eq!(hits_after[0].path, "pkg/sidecar/sidecar.go");

        // Now corrupt the BLAKE3 checksum footer
        drop(e);
        let mut bytes = fs::read(&vectors_path).unwrap();
        let len = bytes.len();
        bytes[len - 1] ^= 0xff;
        fs::write(&vectors_path, &bytes).unwrap();

        let embedder = Arc::new(HashEmbedder::new());
        let mut e = Engine::open_with_embedder(
            dir.path(),
            Config::default(),
            Some(Box::new(SharedEmbedder(embedder.clone()))),
        )
        .unwrap();
        assert_eq!(e.status().unwrap().vectors, 0);

        let r = e.index(false).unwrap();
        assert_eq!(r.embedded, 0);
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);

        let hits_after2 = e.search_dense("LocateBinary sidecar", 1).unwrap();
        assert_eq!(hits_after2[0].path, "pkg/sidecar/sidecar.go");
    }

    #[test]
    fn sync_vectors_detects_mismatched_ids_with_equal_count() {
        let dir = project();
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        e.index(false).unwrap();
        let total = e.status().unwrap().chunks as usize;
        assert!(total > 0);
        let vectors_path = dir.path().join(".dendrite/rag").join(VECTORS_FILE);

        // Replace one valid ID with an alien ID, keeping the same count
        let mut idx = FlatIndex::load(&vectors_path).unwrap();
        let first_id = idx.ids().next().unwrap().to_string();
        let tag = idx.tag(&first_id).unwrap().to_string();
        idx.remove(&first_id);
        idx.upsert("alien-chunk-id", &tag, &vec![0.1f32; 64]);
        assert_eq!(idx.len(), total);
        idx.save(&vectors_path).unwrap();

        // Reopen with this modified vectors.bin
        drop(e);
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        // Count matches, but IDs do not match ledger chunk IDs
        assert_eq!(e.status().unwrap().vectors, total);
        assert!(!e.vectors.contains(&first_id));
        assert!(e.vectors.contains("alien-chunk-id"));

        // index(false) must detect the mismatch, remove alien id, and refill the missing id from cache
        let r = e.index(false).unwrap();
        assert_eq!(r.embedded, 0);
        assert_eq!(e.status().unwrap().vectors, total);
        assert!(e.vectors.contains(&first_id));
        assert!(!e.vectors.contains("alien-chunk-id"));
    }

    #[test]
    fn query_cache_skips_repeated_embeddings() {
        use std::sync::atomic::Ordering;
        let dir = project();
        let embedder = HashEmbedder::new();
        let calls = embedder.calls.clone();
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
        e.index(false).unwrap();

        let initial_calls = calls.load(Ordering::SeqCst);

        // First query embeds and populates the cache.
        let hits1 = e.dense_ranking("LocateBinary sidecar", 1).unwrap();
        assert!(!hits1.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 1);
        assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 1);

        // Second identical query hits the LRU cache without calling the embedder.
        let hits2 = e.dense_ranking("LocateBinary sidecar", 1).unwrap();
        assert_eq!(hits1, hits2);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            initial_calls + 1,
            "repeated query must not call the embedder"
        );

        // A different query calls the embedder again.
        let hits3 = e.dense_ranking("IndexProject", 1).unwrap();
        assert!(!hits3.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);
        assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 2);
    }

    #[test]
    fn query_cache_is_keyed_by_model() {
        use std::sync::atomic::Ordering;
        let dir = project();
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
                .unwrap();
        e.index(false).unwrap();
        e.dense_ranking("LocateBinary", 1).unwrap();
        let other = HashEmbedder::with_model("hash-64-v2");
        let calls = other.calls.clone();
        e.embedder = Some(Box::new(other));
        e.dense_ranking("LocateBinary", 1).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "another model must not reuse the vector"
        );
    }

    #[test]
    fn query_cache_disabled_when_capacity_is_zero() {
        use std::sync::atomic::Ordering;
        let dir = project();
        let embedder = HashEmbedder::new();
        let calls = embedder.calls.clone();
        let cfg = Config {
            query_cache: 0,
            ..Config::default()
        };
        let mut e = Engine::open_with_embedder(dir.path(), cfg, Some(Box::new(embedder))).unwrap();
        e.index(false).unwrap();

        let initial_calls = calls.load(Ordering::SeqCst);

        e.dense_ranking("LocateBinary", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 1);
        assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 0);

        // Second identical query still calls the embedder because cache is disabled.
        e.dense_ranking("LocateBinary", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);
        assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 0);
    }

    #[test]
    fn query_cache_evicts_lru_when_capacity_reached() {
        use std::sync::atomic::Ordering;
        let dir = project();
        let embedder = HashEmbedder::new();
        let calls = embedder.calls.clone();
        let cfg = Config {
            query_cache: 2,
            ..Config::default()
        };
        let mut e = Engine::open_with_embedder(dir.path(), cfg, Some(Box::new(embedder))).unwrap();
        e.index(false).unwrap();

        let initial_calls = calls.load(Ordering::SeqCst);

        e.dense_ranking("q1", 1).unwrap();
        e.dense_ranking("q2", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);
        assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 2);

        // Access q1 so q2 becomes the least recently used.
        e.dense_ranking("q1", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);

        // Insert q3, evicting q2.
        e.dense_ranking("q3", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 3);
        assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 2);

        // q1 is still in cache.
        e.dense_ranking("q1", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 3);

        // q2 was evicted, so it calls the embedder again.
        e.dense_ranking("q2", 1).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 4);
    }

    #[test]
    fn index_stores_communities_per_file_and_chunk() {
        let dir = project();
        let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
        let r = e.index(false).unwrap();
        assert!(r.communities > 0);
        let files = e.ledger.community_files().unwrap();
        assert_eq!(files.len(), e.ledger.files().unwrap().len());
        let (path, id) = files.iter().next().unwrap();
        let entry = &e.ledger.files().unwrap()[path];
        if let Some(chunk) = entry.chunk_ids.first() {
            assert_eq!(e.ledger.community_of_chunk(chunk).unwrap(), Some(*id));
        }
        assert!(e.ledger.community(*id).unwrap().unwrap().files.contains(path));
        assert_eq!(e.status().unwrap().communities, r.communities);
        // A no-op run keeps them without recomputing.
        let again = e.index(false).unwrap();
        assert_eq!(again.communities, r.communities);
        assert_eq!(again.graph_ms, 0);
    }

    #[test]
    fn a_cleared_ledger_rebuilds_the_lexical_index() {
        let dir = project();
        let mut e = open(dir.path());
        e.index(false).unwrap();
        let docs = e.lexical.len();
        drop(e);
        {
            // An older schema version: the next open clears files and chunks.
            let ledger = crate::ledger::Ledger::open(&dir.path().join(".dendrite/rag/ledger.redb")).unwrap();
            let mut b = ledger.batch().unwrap();
            b.set_meta("schema", "1").unwrap();
            b.commit().unwrap();
        }
        let mut e = open(dir.path());
        e.index(false).unwrap();
        assert_eq!(e.lexical.len(), docs, "lexical documents must not be duplicated");
    }

    #[test]
    fn llm_request_shape_and_enrichment() {
        use crate::graph::query::Explanation;
        let dir = project();
        fs::write(
            dir.path().join("pkg/sidecar/client.go"),
            "package sidecar\n\nfunc Start(name string) string { return LocateBinary(name) }\n",
        )
        .unwrap();
        let resp = r#"{"choices":[{"message":{"content":"[{\"name\": \"Testing\", \"description\": \"Test suite concept\"}]"}}]}"#;
        let stub = crate::testutil::HttpStub::serve(resp);
        let cfg = Config {
            graph_llm: crate::settings::GraphLlmSettings {
                enabled: true,
                model: Some("test-model".into()),
                summaries: true,
                concepts: true,
            },
            ..Config::default()
        };
        let mut e = Engine::open_with_embedder(dir.path(), cfg, None)
            .unwrap()
            .with_llm(crate::llm::Client::new(
                Some(stub.url.clone()),
                Some("secret-key".into()),
                "test-model".into(),
            ));
        let r = e.index(false).unwrap();
        assert_eq!(r.llm_error, None);

        // Check captured requests
        assert!(stub.request_count() >= 1);
        let req = stub.request();
        assert!(
            req.head
                .to_ascii_lowercase()
                .contains("authorization: bearer secret-key")
        );
        assert_eq!(req.body["model"], "test-model");
        assert_eq!(req.body["temperature"], 0.0);

        // Verify concepts were extracted and exposed on graph_explain
        let expl = e.graph_explain("README.md").unwrap();
        assert!(!expl.is_empty());
        let Explanation::File { concepts, .. } = &expl[0] else {
            panic!("expected File explanation");
        };
        assert_eq!(concepts.len(), 1);
        assert_eq!(concepts[0].name, "Testing");
        assert_eq!(concepts[0].description, "Test suite concept");

        // Verify summary chunk has the appended text
        let mut found_summary = false;
        e.ledger
            .for_each_chunk(&mut |c| {
                if c.path.starts_with("@community/") {
                    found_summary = true;
                    assert!(c.content.contains("Testing"));
                }
            })
            .unwrap();
        assert!(found_summary);
    }

    #[test]
    fn llm_caching_makes_no_requests_on_second_run() {
        let dir = project();
        let resp = r#"{"choices":[{"message":{"content":"[{\"name\": \"Testing\", \"description\": \"Test suite concept\"}]"}}]}"#;
        let stub = crate::testutil::HttpStub::serve(resp);
        let cfg = Config {
            graph_llm: crate::settings::GraphLlmSettings {
                enabled: true,
                model: Some("test-model".into()),
                summaries: true,
                concepts: true,
            },
            ..Config::default()
        };
        let mut e = Engine::open_with_embedder(dir.path(), cfg, None)
            .unwrap()
            .with_llm(crate::llm::Client::new(
                Some(stub.url.clone()),
                None,
                "test-model".into(),
            ));
        e.index(false).unwrap();
        let initial_count = stub.request_count();
        assert!(initial_count > 0);

        // Second index run: unchanged files and communities
        let r2 = e.index(false).unwrap();
        assert_eq!(r2.llm_error, None);
        assert_eq!(
            stub.request_count(),
            initial_count,
            "second index run must make no requests"
        );

        // Full rebuild: llm_cache persists in redb, so no requests
        let r3 = e.index(true).unwrap();
        assert_eq!(r3.llm_error, None);
        assert_eq!(
            stub.request_count(),
            initial_count,
            "full index rebuild must reuse llm_cache and make no requests"
        );
    }

    #[test]
    fn llm_failure_keeps_deterministic_summary() {
        let dir = project();
        // A second file calling into the first makes a community of two, with a summary.
        fs::write(
            dir.path().join("pkg/sidecar/client.go"),
            "package sidecar\n\nfunc Start(name string) string { return LocateBinary(name) }\n",
        )
        .unwrap();
        let stub = crate::testutil::HttpStub::serve_status(
            500,
            "Internal Server Error",
            r#"{"error": "overloaded"}"#,
        );
        let cfg = Config {
            graph_llm: crate::settings::GraphLlmSettings {
                enabled: true,
                model: Some("test-model".into()),
                summaries: true,
                concepts: true,
            },
            ..Config::default()
        };
        let mut e = Engine::open_with_embedder(dir.path(), cfg, None)
            .unwrap()
            .with_llm(crate::llm::Client::new(
                Some(stub.url.clone()),
                None,
                "test-model".into(),
            ));
        let r = e.index(false).unwrap();
        assert!(r.llm_error.is_some(), "must record llm_error");

        // The deterministic summary chunk is preserved
        let mut found_summary = false;
        e.ledger
            .for_each_chunk(&mut |c| {
                if c.path.starts_with("@community/") {
                    found_summary = true;
                    assert!(c.content.contains("Files:"));
                }
            })
            .unwrap();
        assert!(found_summary);
    }

    #[test]
    fn llm_disabled_makes_no_requests() {
        let dir = project();
        let stub = crate::testutil::HttpStub::serve(r#"{"choices":[]}"#);
        let cfg = Config {
            graph_llm: crate::settings::GraphLlmSettings {
                enabled: false,
                model: Some("test-model".into()),
                summaries: true,
                concepts: true,
            },
            ..Config::default()
        };
        let mut e = Engine::open_with_embedder(dir.path(), cfg, None)
            .unwrap()
            .with_llm(crate::llm::Client::new(
                Some(stub.url.clone()),
                None,
                "test-model".into(),
            ));
        let r = e.index(false).unwrap();
        assert_eq!(r.llm_error, None);
        assert_eq!(stub.request_count(), 0);
    }

    #[test]
    fn llm_config_validation() {
        let dir = project();
        let cfg = Config {
            graph_llm: crate::settings::GraphLlmSettings {
                enabled: true,
                model: None,
                summaries: true,
                concepts: true,
            },
            ..Config::default()
        };
        assert!(Engine::open(dir.path(), cfg).is_err());
    }
}
