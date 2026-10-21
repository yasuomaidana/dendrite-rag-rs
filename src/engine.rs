//! The engine: opens a project's index, keeps it current and answers queries.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use embedag::dag::merkle::{MerkleNode, diff};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chunk::{Chunk, ChunkOptions, chunk_file};
use crate::embed::{EmbedConfig, Embedder};
use crate::ledger::{FileEntry, Ledger};
use crate::lexical::LexicalIndex;
use crate::rerank::{GatewayReranker, RerankConfig, Reranker};
use crate::symbols::SymbolIndex;
use crate::vector::{FlatIndex, VectorIndex};
use crate::walk::{SourceFile, WalkOptions, read_text, walk};

/// Engine configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Where the index lives; defaults to `<root>/.dendrite/rag`.
    pub index_dir: Option<PathBuf>,
    pub walk: WalkOptions,
    pub chunk: ChunkOptions,
    pub embed: EmbedConfig,
    pub rerank: RerankConfig,
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
}

pub struct Engine {
    pub(crate) root: PathBuf,
    pub(crate) dir: PathBuf,
    pub(crate) cfg: Config,
    pub(crate) ledger: Ledger,
    pub(crate) lexical: LexicalIndex,
    pub(crate) embedder: Option<Box<dyn Embedder>>,
    pub(crate) vectors: FlatIndex,
    pub(crate) reranker: Option<Box<dyn Reranker>>,
    /// Built lazily from the ledger; cleared whenever an index run changes chunks.
    pub(crate) symbols: RwLock<Option<Arc<SymbolIndex>>>,
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
        std::fs::create_dir_all(&dir)?;
        // The index is per-machine state: keep it out of version control.
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(ignore, "*\n")?;
        }
        let ledger = Ledger::open(&dir.join("ledger.redb"))?;
        let lexical = LexicalIndex::open(&dir.join("tantivy"))?;
        let mut vectors = FlatIndex::load(&dir.join(VECTORS_FILE))?;
        // Vectors from another model are useless: rebuild them (from the cache when possible).
        if ledger.meta("embed_model")?.as_deref() != embedder.as_ref().map(|e| e.model_id()) {
            vectors.clear();
        }
        Ok(Self {
            root,
            dir,
            ledger,
            lexical,
            reranker: GatewayReranker::new(&cfg.rerank).map(|r| Box::new(r) as Box<dyn Reranker>),
            cfg,
            embedder,
            vectors,
            symbols: RwLock::new(None),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
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
        let fresh: Vec<(&SourceFile, &String, Vec<Chunk>)> = scanned
            .par_iter()
            .filter(|(f, _)| added.contains(&f.path))
            .map(|(f, h)| {
                let chunks =
                    read_text(&f.abs).map_or_else(Vec::new, |src| chunk_file(&f.path, f.lang, &src, opts));
                (f, h, chunks)
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
        if full {
            lw.clear()?;
        }
        for p in &removed {
            lw.remove_file(p);
        }
        for (_, _, chunks) in &fresh {
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
            }
        }
        for (f, h, chunks) in &fresh {
            for c in chunks {
                batch.put_chunk(c)?;
            }
            report.chunks_added += chunks.len();
            let entry = FileEntry {
                hash: (*h).clone(),
                size: f.size,
                mtime_ns: f.mtime_ns,
                chunk_ids: chunks.iter().map(|c| c.id.clone()).collect(),
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
        }
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
        let path = self.dir.join(VECTORS_FILE);
        let Some(embedder) = &self.embedder else {
            if !self.vectors.is_empty() {
                self.vectors.clear();
                self.vectors.save(&path)?;
            }
            return Ok(());
        };
        let model = embedder.model_id().to_string();
        let chunk_count = self.ledger.chunk_count()? as usize;
        let same_model = self.ledger.meta("embed_model")?.as_deref() == Some(model.as_str());
        if !changed && same_model && self.vectors.len() == chunk_count {
            return Ok(());
        }

        // Only ids and content hashes are kept in memory; texts are loaded per group.
        let mut live: BTreeMap<String, String> = BTreeMap::new();
        self.ledger.for_each_chunk(|c| {
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
        self.vectors.save(&path)?;
        Ok(())
    }

    /// Dense-only search; empty when no embedder is configured.
    pub fn search_dense(&self, query: &str, top_k: usize) -> Result<Vec<Hit>> {
        let ranked = self.dense_ranking(query, top_k)?;
        self.hits(&ranked)
    }

    /// Embeds a query with the configured embedder (`None` without one).
    pub fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
        match &self.embedder {
            Some(e) => Ok(Some(e.embed_query(query)?)),
            None => Ok(None),
        }
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

    /// `(chunk_id, cosine)` for the query, best first.
    pub fn dense_ranking(&self, query: &str, top_k: usize) -> Result<Vec<(String, f32)>> {
        let Some(embedder) = &self.embedder else {
            return Ok(Vec::new());
        };
        if self.vectors.is_empty() {
            return Ok(Vec::new());
        }
        let q = embedder.embed_query(query)?;
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

const VECTORS_FILE: &str = "vectors.bin";
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
    use crate::testutil::{HashEmbedder, project};
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
}
