//! Edge mode (`docs/specs/store.md` § Edge mode): the local store serves every read and
//! write, every commit is mirrored to the shared PostgreSQL tier, and a commit made while the
//! tier is unreachable goes to a write journal that is replayed, in order, before the next
//! mirror. Embeddings missing locally are read through from the shared tier first, so a fresh
//! checkout reuses what the team has already embedded.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::pg::{LedgerOp, LexicalOp, Pg, PgLedger, SyncBatch, VectorOp};
use super::{LedgerStore, LedgerWrite, LexicalStore, LexicalWrite, Store};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::FileEntry;
use crate::vector::VectorIndex;

/// How often an unreachable shared tier is tried again.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// The journal's name inside the index directory (kept out of version control with it).
pub const JOURNAL_FILE: &str = "memory-wal.journal";

/// The connection to the shared tier and the journal of commits it has not received.
pub(crate) struct Edge {
    url: String,
    repo: String,
    journal: PathBuf,
    pg: Mutex<Option<Arc<Pg>>>,
    last_attempt: Mutex<Option<Instant>>,
    /// Serializes mirroring and replay, so journal order is commit order.
    sending: Mutex<()>,
}

impl Edge {
    pub(crate) fn new(url: &str, repo: &str, journal: PathBuf) -> Arc<Self> {
        let edge = Arc::new(Self {
            url: url.to_string(),
            repo: repo.to_string(),
            journal,
            pg: Mutex::new(None),
            last_attempt: Mutex::new(None),
            sending: Mutex::new(()),
        });
        let _ = edge.replay();
        edge
    }

    /// The shared tier, connecting at most once per [`RETRY_INTERVAL`] while it is down.
    fn remote(&self) -> Option<Arc<Pg>> {
        let mut pg = self.pg.lock().expect("edge connection lock");
        if pg.is_none() {
            let mut last = self.last_attempt.lock().expect("edge attempt lock");
            if last.is_none_or(|t| t.elapsed() >= RETRY_INTERVAL) {
                *last = Some(Instant::now());
                *pg = Pg::connect(&self.url, &self.repo).ok().map(Arc::new);
            }
        }
        pg.clone()
    }

    /// Forgets a connection that failed; the next use reconnects after the retry interval.
    fn lost(&self) {
        *self.pg.lock().expect("edge connection lock") = None;
        *self.last_attempt.lock().expect("edge attempt lock") = Some(Instant::now());
    }

    /// Mirrors one commit, after any journaled ones; journals it when the tier is down.
    fn send(&self, batch: SyncBatch) -> std::io::Result<()> {
        let line = serde_json::to_string(&batch).map_err(std::io::Error::other)?;
        let _order = self.sending.lock().expect("edge send lock");
        if self.replay_locked().is_ok()
            && let Some(pg) = self.remote()
        {
            if pg.apply(batch).is_ok() {
                return Ok(());
            }
            self.lost();
        }
        self.append(&line)
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.journal)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()
    }

    /// Replays the journal into the shared tier; returns how many commits are still pending.
    pub(crate) fn replay(&self) -> std::io::Result<usize> {
        let _order = self.sending.lock().expect("edge send lock");
        match self.replay_locked() {
            Ok(()) => Ok(0),
            Err(pending) => Ok(pending),
        }
    }

    /// `Err(pending)` when the journal could not be emptied.
    fn replay_locked(&self) -> Result<(), usize> {
        let text = std::fs::read_to_string(&self.journal).unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            return Ok(());
        }
        let Some(pg) = self.remote() else {
            return Err(lines.len());
        };
        for (i, line) in lines.iter().enumerate() {
            // A line that does not parse was cut by a crash mid-write; it never reached
            // the local store's commit either, so it is dropped.
            let Ok(batch) = serde_json::from_str::<SyncBatch>(line) else {
                continue;
            };
            if pg.apply(batch).is_err() {
                self.lost();
                let rest = lines[i..].join("\n") + "\n";
                let _ = rewrite(&self.journal, &rest);
                return Err(lines.len() - i);
            }
        }
        let _ = rewrite(&self.journal, "");
        Ok(())
    }

    /// Commits waiting in the journal.
    pub(crate) fn pending(&self) -> usize {
        std::fs::read_to_string(&self.journal)
            .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }
}

/// Replaces the journal's content atomically (temporary file, fsync, rename).
fn rewrite(path: &PathBuf, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

/// Wraps a local store for edge mode.
pub(crate) fn wrap(local: Store, edge: Arc<Edge>) -> Store {
    Store {
        ledger: Box::new(EdgeLedger {
            local: local.ledger,
            edge: edge.clone(),
        }),
        lexical: Box::new(EdgeLexical {
            local: local.lexical,
            edge: edge.clone(),
        }),
        vectors: Box::new(EdgeVectors {
            local: local.vectors,
            pending: Mutex::new(Vec::new()),
            edge,
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// Ledger

struct EdgeLedger {
    local: Box<dyn LedgerStore>,
    edge: Arc<Edge>,
}

impl LedgerStore for EdgeLedger {
    fn files(&self) -> crate::ledger::Result<BTreeMap<String, FileEntry>> {
        self.local.files()
    }

    fn chunk(&self, id: &str) -> crate::ledger::Result<Option<Chunk>> {
        self.local.chunk(id)
    }

    fn chunks(&self, ids: &[String]) -> crate::ledger::Result<Vec<Chunk>> {
        self.local.chunks(ids)
    }

    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> crate::ledger::Result<()> {
        self.local.for_each_chunk(f)
    }

    fn chunk_count(&self) -> crate::ledger::Result<u64> {
        self.local.chunk_count()
    }

    fn meta(&self, key: &str) -> crate::ledger::Result<Option<String>> {
        self.local.meta(key)
    }

    fn embedding(&self, key: &str) -> crate::ledger::Result<Option<Vec<f32>>> {
        Ok(self.embeddings(&[key.to_string()])?.pop().flatten())
    }

    /// Local cache first; misses are read from the shared tier and cached locally.
    fn embeddings(&self, keys: &[String]) -> crate::ledger::Result<Vec<Option<Vec<f32>>>> {
        let mut out = self.local.embeddings(keys)?;
        let missing: Vec<usize> = (0..keys.len()).filter(|&i| out[i].is_none()).collect();
        if missing.is_empty() {
            return Ok(out);
        }
        let Some(pg) = self.edge.remote() else {
            return Ok(out);
        };
        let wanted: Vec<String> = missing.iter().map(|&i| keys[i].clone()).collect();
        match PgLedger(pg).embeddings(&wanted) {
            Ok(found) => {
                let mut batch = self.local.batch()?;
                for (&i, v) in missing.iter().zip(found) {
                    if let Some(v) = v {
                        batch.put_embedding(&keys[i], &v)?;
                        out[i] = Some(v);
                    }
                }
                batch.commit()?;
            }
            Err(_) => self.edge.lost(),
        }
        Ok(out)
    }

    fn embedding_count(&self) -> crate::ledger::Result<u64> {
        self.local.embedding_count()
    }

    fn community_files(&self) -> crate::ledger::Result<BTreeMap<String, u32>> {
        self.local.community_files()
    }

    fn community_of_chunk(&self, chunk: &str) -> crate::ledger::Result<Option<u32>> {
        self.local.community_of_chunk(chunk)
    }

    fn community(&self, id: u32) -> crate::ledger::Result<Option<Community>> {
        self.local.community(id)
    }

    fn communities(&self) -> crate::ledger::Result<Vec<Community>> {
        self.local.communities()
    }

    fn llm_cache(&self, model: &str, prompt_hash: &str) -> crate::ledger::Result<Option<String>> {
        self.local.llm_cache(model, prompt_hash)
    }

    fn concepts(&self, path: &str) -> crate::ledger::Result<Option<String>> {
        self.local.concepts(path)
    }

    fn sync_pending(&self) -> Option<usize> {
        Some(self.edge.pending())
    }

    fn sync_now(&self) -> crate::ledger::Result<usize> {
        self.edge
            .replay()
            .map_err(|e| crate::ledger::LedgerError::Backend(e.to_string()))
    }

    fn batch(&self) -> crate::ledger::Result<Box<dyn LedgerWrite + '_>> {
        Ok(Box::new(EdgeLedgerWrite {
            local: self.local.batch()?,
            ops: Vec::new(),
            edge: &self.edge,
        }))
    }
}

struct EdgeLedgerWrite<'a> {
    local: Box<dyn LedgerWrite + 'a>,
    ops: Vec<LedgerOp>,
    edge: &'a Edge,
}

impl LedgerWrite for EdgeLedgerWrite<'_> {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> crate::ledger::Result<()> {
        self.local.put_file(path, entry)?;
        self.ops
            .push(LedgerOp::PutFile(path.to_string(), serde_json::to_value(entry)?));
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> crate::ledger::Result<()> {
        self.local.remove_file(path)?;
        self.ops.push(LedgerOp::RemoveFile(path.to_string()));
        Ok(())
    }

    fn put_chunk(&mut self, chunk: &Chunk) -> crate::ledger::Result<()> {
        self.local.put_chunk(chunk)?;
        self.ops.push(LedgerOp::PutChunk(
            chunk.id.clone(),
            chunk.path.clone(),
            serde_json::to_value(chunk)?,
        ));
        Ok(())
    }

    fn remove_chunk(&mut self, id: &str) -> crate::ledger::Result<()> {
        self.local.remove_chunk(id)?;
        self.ops.push(LedgerOp::RemoveChunk(id.to_string()));
        Ok(())
    }

    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> crate::ledger::Result<()> {
        self.local.put_embedding(key, vector)?;
        let (model, hash) = key.split_once('\0').unwrap_or(("", key));
        self.ops.push(LedgerOp::PutEmbedding(
            model.to_string(),
            hash.to_string(),
            vector.to_vec(),
        ));
        Ok(())
    }

    fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> crate::ledger::Result<()> {
        self.local.put_llm_cache(model, prompt_hash, value)?;
        self.ops.push(LedgerOp::PutLlmCache(
            model.to_string(),
            prompt_hash.to_string(),
            value.to_string(),
        ));
        Ok(())
    }

    fn put_concepts(&mut self, path: &str, json: &str) -> crate::ledger::Result<()> {
        self.local.put_concepts(path, json)?;
        self.ops
            .push(LedgerOp::PutConcepts(path.to_string(), json.to_string()));
        Ok(())
    }

    fn remove_concepts(&mut self, path: &str) -> crate::ledger::Result<()> {
        self.local.remove_concepts(path)?;
        self.ops.push(LedgerOp::RemoveConcepts(path.to_string()));
        Ok(())
    }

    fn set_meta(&mut self, key: &str, value: &str) -> crate::ledger::Result<()> {
        self.local.set_meta(key, value)?;
        self.ops
            .push(LedgerOp::SetMeta(key.to_string(), value.to_string()));
        Ok(())
    }

    fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> crate::ledger::Result<()> {
        self.local.put_communities(communities, files)?;
        self.ops.push(LedgerOp::communities(communities, files)?);
        Ok(())
    }

    fn clear_index(&mut self) -> crate::ledger::Result<()> {
        self.local.clear_index()?;
        self.ops.push(LedgerOp::ClearIndex);
        Ok(())
    }

    fn commit(self: Box<Self>) -> crate::ledger::Result<()> {
        let EdgeLedgerWrite { local, ops, edge } = *self;
        local.commit()?;
        if !ops.is_empty() {
            edge.send(SyncBatch::Ledger(ops))
                .map_err(|e| crate::ledger::LedgerError::Backend(e.to_string()))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Lexical

struct EdgeLexical {
    local: Box<dyn LexicalStore>,
    edge: Arc<Edge>,
}

impl LexicalStore for EdgeLexical {
    fn search(&self, query: &str, limit: usize) -> tantivy::Result<Vec<(String, f32)>> {
        self.local.search(query, limit)
    }

    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>> {
        Ok(Box::new(EdgeLexicalWrite {
            local: self.local.writer()?,
            ops: Vec::new(),
            edge: &self.edge,
        }))
    }

    fn len(&self) -> u64 {
        self.local.len()
    }
}

struct EdgeLexicalWrite<'a> {
    local: Box<dyn LexicalWrite + 'a>,
    ops: Vec<LexicalOp>,
    edge: &'a Edge,
}

impl LexicalWrite for EdgeLexicalWrite<'_> {
    fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()> {
        self.local.add(chunk)?;
        self.ops.push(LexicalOp::Add(Box::new(chunk.clone())));
        Ok(())
    }

    fn remove_file(&mut self, path: &str) {
        self.local.remove_file(path);
        self.ops.push(LexicalOp::RemoveFile(path.to_string()));
    }

    fn clear(&mut self) -> tantivy::Result<()> {
        self.local.clear()?;
        self.ops.push(LexicalOp::Clear);
        Ok(())
    }

    fn commit(self: Box<Self>) -> tantivy::Result<()> {
        let EdgeLexicalWrite { local, ops, edge } = *self;
        local.commit()?;
        if !ops.is_empty() {
            edge.send(SyncBatch::Lexical(ops))
                .map_err(|e| tantivy::TantivyError::SystemError(format!("edge journal: {e}")))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Vectors

struct EdgeVectors {
    local: Box<dyn VectorIndex>,
    pending: Mutex<Vec<VectorOp>>,
    edge: Arc<Edge>,
}

impl EdgeVectors {
    fn push(&self, op: VectorOp) {
        self.pending.lock().expect("edge vectors lock").push(op);
    }
}

impl VectorIndex for EdgeVectors {
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]) {
        self.local.upsert(id, tag, vector);
        self.push(VectorOp::Upsert(id.to_string(), tag.to_string(), vector.to_vec()));
    }

    fn remove(&mut self, id: &str) -> bool {
        self.push(VectorOp::Remove(id.to_string()));
        self.local.remove(id)
    }

    fn contains(&self, id: &str) -> bool {
        self.local.contains(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        self.local.search(query, k)
    }

    fn len(&self) -> usize {
        self.local.len()
    }

    fn dim(&self) -> usize {
        self.local.dim()
    }

    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        self.local.ids()
    }

    fn tag(&self, id: &str) -> Option<&str> {
        self.local.tag(id)
    }

    fn clear(&mut self) {
        self.local.clear();
        self.push(VectorOp::Clear);
    }

    fn persist(&self) -> std::io::Result<()> {
        self.local.persist()?;
        let ops = std::mem::take(&mut *self.pending.lock().expect("edge vectors lock"));
        if ops.is_empty() {
            return Ok(());
        }
        self.edge.send(SyncBatch::Vectors(ops))
    }
}

/// Run against a scratch database: `DENDRITE_RAG_TEST_PG_URL=postgresql://… cargo test
/// --features postgres`. Without the variable they pass without doing anything.
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::embed::Embedder;
    use crate::engine::{Config, Engine};
    use crate::store::{StorageConfig, StorageMode};
    use crate::testutil::{HashEmbedder, project};

    struct Shared(Arc<HashEmbedder>);

    impl Embedder for Shared {
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

    fn url() -> Option<String> {
        std::env::var("DENDRITE_RAG_TEST_PG_URL")
            .ok()
            .filter(|u| !u.is_empty())
    }

    fn unique(prefix: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{prefix}-{nanos}")
    }

    fn open(
        root: &std::path::Path,
        index: &str,
        mode: StorageMode,
        url: &str,
        repo: &str,
        e: &Arc<HashEmbedder>,
    ) -> Engine {
        let cfg = Config {
            index_dir: Some(root.join(index)),
            storage: StorageConfig {
                mode,
                repo: Some(repo.to_string()),
                pg_url: Some(url.to_string()),
            },
            ..Config::default()
        };
        Engine::open_with_embedder(root, cfg, Some(Box::new(Shared(e.clone())))).unwrap()
    }

    #[test]
    fn journals_while_down_and_replays_once_up() {
        let Some(url) = url() else { return };
        let dir = project();
        let repo = unique("edge");
        let embedder = Arc::new(HashEmbedder::new());
        // Nothing listens on port 1: every mirror goes to the journal.
        let mut down = open(
            dir.path(),
            ".dendrite/rag-edge",
            StorageMode::Edge,
            "postgresql://rag:rag@127.0.0.1:1/x",
            &repo,
            &embedder,
        );
        down.index(false).unwrap();
        let pending = down.status().unwrap().sync_pending.unwrap();
        assert!(
            pending >= 3,
            "ledger, lexical and vector commits are journaled: {pending}"
        );
        let local = down.status().unwrap();
        drop(down);

        // Back up: opening replays the journal, in order, into the shared tier.
        let up = open(
            dir.path(),
            ".dendrite/rag-edge",
            StorageMode::Edge,
            &url,
            &repo,
            &embedder,
        );
        assert_eq!(up.status().unwrap().sync_pending, Some(0));
        assert_eq!(up.sync().unwrap(), 0, "a second replay has nothing to send");
        let remote = open(
            dir.path(),
            ".dendrite/rag-remote",
            StorageMode::Remote,
            &url,
            &repo,
            &embedder,
        );
        let shared = remote.status().unwrap();
        assert_eq!(shared.chunks, local.chunks);
        assert_eq!(shared.vectors, local.vectors);
        assert_eq!(shared.communities, local.communities);
    }

    #[test]
    fn reads_embeddings_through_from_the_shared_tier() {
        let Some(url) = url() else { return };
        let dir = project();
        // Someone else's machine indexed the same content into the shared tier.
        let first = Arc::new(HashEmbedder::new());
        let mut theirs = open(
            dir.path(),
            ".dendrite/rag-a",
            StorageMode::Edge,
            &url,
            &unique("theirs"),
            &first,
        );
        theirs.index(false).unwrap();
        // A fresh checkout in edge mode embeds nothing: every vector comes from the tier.
        let second = Arc::new(HashEmbedder::new());
        let mut ours = open(
            dir.path(),
            ".dendrite/rag-b",
            StorageMode::Edge,
            &url,
            &unique("ours"),
            &second,
        );
        let r = ours.index(false).unwrap();
        assert_eq!(r.embedded, 0);
        assert_eq!(second.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            ours.status().unwrap().vectors as u64,
            ours.status().unwrap().chunks
        );
        let p = ours.pull().unwrap();
        assert_eq!(p.cached, p.chunks);
    }
}
