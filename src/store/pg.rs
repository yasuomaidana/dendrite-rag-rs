//! PostgreSQL storage, the shared tier (`docs/specs/store.md` § PostgreSQL): the ledger,
//! the lexical index (BM25 computed in SQL over postings of the code tokenizer) and the
//! vectors (pgvector), for every repository of a team in one database.
//!
//! Every table is keyed by repository, except the embedding cache, which is keyed by model
//! and content hash only: an embedding computed on one machine serves every repository and
//! every machine. Writes are buffered and applied in one transaction on commit, and every
//! statement is an upsert or a keyed delete, so replaying an interrupted run is harmless.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use pgvector::Vector;
use postgres::{Client, NoTls, Transaction};
use serde_json::Value;

use super::{LedgerStore, LedgerWrite, LexicalStore, LexicalWrite, Store};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, LedgerError};
use crate::lexical::{code_terms, sanitize};
use crate::vector::{FlatIndex, VectorIndex};

/// Schema version recorded in `rag_schema`.
pub const SCHEMA_VERSION: i32 = 1;

const MIGRATION: &str = r#"
CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE IF NOT EXISTS rag_schema (version int NOT NULL);
CREATE TABLE IF NOT EXISTS rag_repos (id serial PRIMARY KEY, name text UNIQUE NOT NULL);
CREATE TABLE IF NOT EXISTS rag_files (
    repo int NOT NULL, path text NOT NULL, entry jsonb NOT NULL, PRIMARY KEY (repo, path));
CREATE TABLE IF NOT EXISTS rag_chunks (
    repo int NOT NULL, id text NOT NULL, path text NOT NULL, chunk jsonb NOT NULL,
    PRIMARY KEY (repo, id));
CREATE INDEX IF NOT EXISTS rag_chunks_path ON rag_chunks (repo, path);
CREATE TABLE IF NOT EXISTS rag_embeddings (
    model text NOT NULL, hash text NOT NULL, vec vector NOT NULL, PRIMARY KEY (model, hash));
CREATE TABLE IF NOT EXISTS rag_meta (
    repo int NOT NULL, key text NOT NULL, value text NOT NULL, PRIMARY KEY (repo, key));
CREATE TABLE IF NOT EXISTS rag_vectors (
    repo int NOT NULL, id text NOT NULL, tag text NOT NULL, vec vector NOT NULL,
    PRIMARY KEY (repo, id));
CREATE TABLE IF NOT EXISTS rag_community_files (
    repo int NOT NULL, path text NOT NULL, cid int NOT NULL, PRIMARY KEY (repo, path));
CREATE TABLE IF NOT EXISTS rag_community_chunks (
    repo int NOT NULL, chunk text NOT NULL, cid int NOT NULL, PRIMARY KEY (repo, chunk));
CREATE TABLE IF NOT EXISTS rag_communities (
    repo int NOT NULL, id int NOT NULL, record jsonb NOT NULL, PRIMARY KEY (repo, id));
CREATE TABLE IF NOT EXISTS rag_docs (
    repo int NOT NULL, chunk text NOT NULL, path text NOT NULL,
    len_text int NOT NULL, len_symbols int NOT NULL, len_path int NOT NULL,
    PRIMARY KEY (repo, chunk));
CREATE INDEX IF NOT EXISTS rag_docs_path ON rag_docs (repo, path);
CREATE TABLE IF NOT EXISTS rag_postings (
    repo int NOT NULL, term text NOT NULL, field smallint NOT NULL, chunk text NOT NULL,
    tf int NOT NULL, PRIMARY KEY (repo, term, field, chunk));
CREATE INDEX IF NOT EXISTS rag_postings_chunk ON rag_postings (repo, chunk);
CREATE TABLE IF NOT EXISTS rag_llm_cache (
    model text NOT NULL, prompt_hash text NOT NULL, value text NOT NULL,
    PRIMARY KEY (model, prompt_hash));
CREATE TABLE IF NOT EXISTS rag_concepts (
    repo int NOT NULL, path text NOT NULL, concepts text NOT NULL, PRIMARY KEY (repo, path));
"#;

/// Okapi BM25 over the postings, the same formula and field boosts as the Tantivy index.
/// `stats` must stay MATERIALIZED: inlined, the planner re-aggregates it for every posting
/// (500 ms instead of 5 ms on 5k chunks).
///
/// text (field 0) ×1, symbols (field 1) ×3, path (field 2) ×1.5; k1 = 1.2, b = 0.75.
const BM25: &str = r#"
WITH q AS (SELECT DISTINCT unnest($2::text[]) AS term),
stats AS MATERIALIZED (
    SELECT count(*)::float8 AS n,
           coalesce(avg(len_text), 0)::float8 AS a0,
           coalesce(avg(len_symbols), 0)::float8 AS a1,
           coalesce(avg(len_path), 0)::float8 AS a2
    FROM rag_docs WHERE repo = $1),
df AS (
    SELECT p.field, p.term, count(*)::float8 AS df
    FROM rag_postings p JOIN q ON q.term = p.term
    WHERE p.repo = $1 GROUP BY p.field, p.term)
SELECT p.chunk, sum(
    (CASE p.field WHEN 0 THEN 1.0 WHEN 1 THEN 3.0 ELSE 1.5 END)
    * ln(1 + (s.n - df.df + 0.5) / (df.df + 0.5))
    * (p.tf * 2.2) / (p.tf + 1.2 * (0.25 + 0.75 * (CASE p.field
        WHEN 0 THEN d.len_text / nullif(s.a0, 0)
        WHEN 1 THEN d.len_symbols / nullif(s.a1, 0)
        ELSE d.len_path / nullif(s.a2, 0) END)))
)::float4 AS score
FROM rag_postings p
JOIN df ON df.field = p.field AND df.term = p.term
JOIN rag_docs d ON d.repo = p.repo AND d.chunk = p.chunk
CROSS JOIN stats s
WHERE p.repo = $1
GROUP BY p.chunk
ORDER BY score DESC, p.chunk
LIMIT $3
"#;

/// Connections shared by the ledger, lexical and vector parts of a project's store.
const POOL: usize = 4;
/// A lexical commit adding more documents than this refreshes the planner statistics.
const ANALYZE_AFTER: usize = 500;

struct Pg {
    clients: Vec<Mutex<Client>>,
    next: AtomicUsize,
    repo: i32,
}

impl Pg {
    /// A free connection if there is one, else the next in turn: concurrent readers do not
    /// queue behind each other.
    fn client(&self) -> MutexGuard<'_, Client> {
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        for i in 0..self.clients.len() {
            if let Ok(c) = self.clients[(start + i) % self.clients.len()].try_lock() {
                return c;
            }
        }
        self.clients[start % self.clients.len()]
            .lock()
            .expect("postgres client lock")
    }
}

fn backend(e: impl std::fmt::Display) -> LedgerError {
    LedgerError::Backend(e.to_string())
}

fn lexical_err(e: impl std::fmt::Display) -> tantivy::TantivyError {
    tantivy::TantivyError::SystemError(format!("postgres: {e}"))
}

/// Opens (and migrates) the shared tier at `url` for repository `repo`.
pub fn open(url: &str, repo: &str) -> crate::engine::Result<Store> {
    let other = |e: postgres::Error| crate::engine::Error::Other(anyhow::anyhow!("postgres: {e}"));
    let mut client = Client::connect(url, NoTls).map_err(other)?;
    client.batch_execute(MIGRATION).map_err(other)?;
    let version: Option<i32> = client
        .query_opt("SELECT version FROM rag_schema LIMIT 1", &[])
        .map_err(other)?
        .map(|r| r.get(0));
    match version {
        None => {
            client
                .execute("INSERT INTO rag_schema (version) VALUES ($1)", &[&SCHEMA_VERSION])
                .map_err(other)?;
        }
        Some(v) if v > SCHEMA_VERSION => {
            return Err(crate::engine::Error::Other(anyhow::anyhow!(
                "postgres schema version {v} is newer than this dendrite-rag ({SCHEMA_VERSION})"
            )));
        }
        Some(_) => {}
    }
    let repo_id: i32 = client
        .query_one(
            "INSERT INTO rag_repos (name) VALUES ($1)
             ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
            &[&repo],
        )
        .map_err(other)?
        .get(0);
    let mut flat = FlatIndex::new();
    for row in client
        .query(
            "SELECT id, tag, vec FROM rag_vectors WHERE repo = $1 ORDER BY id",
            &[&repo_id],
        )
        .map_err(other)?
    {
        let v: Vector = row.get(2);
        flat.upsert(row.get(0), row.get(1), v.as_slice());
    }
    let mut clients = vec![Mutex::new(client)];
    for _ in 1..POOL {
        clients.push(Mutex::new(Client::connect(url, NoTls).map_err(other)?));
    }
    let pg = Arc::new(Pg {
        clients,
        next: AtomicUsize::new(0),
        repo: repo_id,
    });
    Ok(Store {
        ledger: Box::new(PgLedger(pg.clone())),
        lexical: Box::new(PgLexical(pg.clone())),
        vectors: Box::new(PgVectors {
            pg,
            flat,
            pending: Mutex::new(Vec::new()),
        }),
    })
}

/// `model ‖ 0 ‖ content_hash` (Postgres text cannot hold the NUL byte, so it is split).
fn split_key(key: &str) -> (&str, &str) {
    key.split_once('\0').unwrap_or(("", key))
}

// ---------------------------------------------------------------------------------------------
// Ledger

struct PgLedger(Arc<Pg>);

impl LedgerStore for PgLedger {
    fn files(&self) -> crate::ledger::Result<BTreeMap<String, FileEntry>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT path, entry FROM rag_files WHERE repo = $1",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        rows.into_iter()
            .map(|r| Ok((r.get(0), serde_json::from_value(r.get::<_, Value>(1))?)))
            .collect()
    }

    fn chunk(&self, id: &str) -> crate::ledger::Result<Option<Chunk>> {
        let row = self
            .0
            .client()
            .query_opt(
                "SELECT chunk FROM rag_chunks WHERE repo = $1 AND id = $2",
                &[&self.0.repo, &id],
            )
            .map_err(backend)?;
        Ok(match row {
            Some(r) => Some(serde_json::from_value(r.get::<_, Value>(0))?),
            None => None,
        })
    }

    fn chunks(&self, ids: &[String]) -> crate::ledger::Result<Vec<Chunk>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT id, chunk FROM rag_chunks WHERE repo = $1 AND id = ANY($2)",
                &[&self.0.repo, &ids],
            )
            .map_err(backend)?;
        let mut by_id: HashMap<String, Value> = rows.into_iter().map(|r| (r.get(0), r.get(1))).collect();
        ids.iter()
            .filter_map(|id| by_id.remove(id))
            .map(|v| Ok(serde_json::from_value(v)?))
            .collect()
    }

    fn for_each_chunk(&self, f: &mut dyn FnMut(Chunk)) -> crate::ledger::Result<()> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT chunk FROM rag_chunks WHERE repo = $1 ORDER BY id",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        for r in rows {
            f(serde_json::from_value(r.get::<_, Value>(0))?);
        }
        Ok(())
    }

    fn chunk_count(&self) -> crate::ledger::Result<u64> {
        let n: i64 = self
            .0
            .client()
            .query_one("SELECT count(*) FROM rag_chunks WHERE repo = $1", &[&self.0.repo])
            .map_err(backend)?
            .get(0);
        Ok(n as u64)
    }

    fn meta(&self, key: &str) -> crate::ledger::Result<Option<String>> {
        Ok(self
            .0
            .client()
            .query_opt(
                "SELECT value FROM rag_meta WHERE repo = $1 AND key = $2",
                &[&self.0.repo, &key],
            )
            .map_err(backend)?
            .map(|r| r.get(0)))
    }

    fn embedding(&self, key: &str) -> crate::ledger::Result<Option<Vec<f32>>> {
        Ok(self.embeddings(&[key.to_string()])?.pop().flatten())
    }

    fn embeddings(&self, keys: &[String]) -> crate::ledger::Result<Vec<Option<Vec<f32>>>> {
        let (models, hashes): (Vec<&str>, Vec<&str>) = keys.iter().map(|k| split_key(k)).unzip();
        let rows = self
            .0
            .client()
            .query(
                "SELECT e.model, e.hash, e.vec
                 FROM unnest($1::text[], $2::text[]) AS k(model, hash)
                 JOIN rag_embeddings e ON e.model = k.model AND e.hash = k.hash",
                &[&models, &hashes],
            )
            .map_err(backend)?;
        let mut found: HashMap<(String, String), Vec<f32>> = rows
            .into_iter()
            .map(|r| ((r.get(0), r.get(1)), r.get::<_, Vector>(2).to_vec()))
            .collect();
        Ok(keys
            .iter()
            .map(|k| {
                let (m, h) = split_key(k);
                found.remove(&(m.to_string(), h.to_string()))
            })
            .collect())
    }

    fn embedding_count(&self) -> crate::ledger::Result<u64> {
        let n: i64 = self
            .0
            .client()
            .query_one("SELECT count(*) FROM rag_embeddings", &[])
            .map_err(backend)?
            .get(0);
        Ok(n as u64)
    }

    fn community_files(&self) -> crate::ledger::Result<BTreeMap<String, u32>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT path, cid FROM rag_community_files WHERE repo = $1",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get(0), r.get::<_, i32>(1) as u32))
            .collect())
    }

    fn community_of_chunk(&self, chunk: &str) -> crate::ledger::Result<Option<u32>> {
        Ok(self
            .0
            .client()
            .query_opt(
                "SELECT cid FROM rag_community_chunks WHERE repo = $1 AND chunk = $2",
                &[&self.0.repo, &chunk],
            )
            .map_err(backend)?
            .map(|r| r.get::<_, i32>(0) as u32))
    }

    fn community(&self, id: u32) -> crate::ledger::Result<Option<Community>> {
        let row = self
            .0
            .client()
            .query_opt(
                "SELECT record FROM rag_communities WHERE repo = $1 AND id = $2",
                &[&self.0.repo, &(id as i32)],
            )
            .map_err(backend)?;
        Ok(match row {
            Some(r) => Some(serde_json::from_value(r.get::<_, Value>(0))?),
            None => None,
        })
    }

    fn communities(&self) -> crate::ledger::Result<Vec<Community>> {
        let rows = self
            .0
            .client()
            .query(
                "SELECT record FROM rag_communities WHERE repo = $1 ORDER BY id",
                &[&self.0.repo],
            )
            .map_err(backend)?;
        rows.into_iter()
            .map(|r| Ok(serde_json::from_value(r.get::<_, Value>(0))?))
            .collect()
    }

    fn llm_cache(&self, model: &str, prompt_hash: &str) -> crate::ledger::Result<Option<String>> {
        Ok(self
            .0
            .client()
            .query_opt(
                "SELECT value FROM rag_llm_cache WHERE model = $1 AND prompt_hash = $2",
                &[&model, &prompt_hash],
            )
            .map_err(backend)?
            .map(|r| r.get(0)))
    }

    fn concepts(&self, path: &str) -> crate::ledger::Result<Option<String>> {
        Ok(self
            .0
            .client()
            .query_opt(
                "SELECT concepts FROM rag_concepts WHERE repo = $1 AND path = $2",
                &[&self.0.repo, &path],
            )
            .map_err(backend)?
            .map(|r| r.get(0)))
    }

    fn batch(&self) -> crate::ledger::Result<Box<dyn LedgerWrite + '_>> {
        Ok(Box::new(PgLedgerWrite {
            pg: &self.0,
            ops: Vec::new(),
        }))
    }
}

enum LedgerOp {
    PutFile(String, Value),
    RemoveFile(String),
    PutChunk(String, String, Value),
    RemoveChunk(String),
    PutEmbedding(String, String, Vector),
    SetMeta(String, String),
    PutLlmCache(String, String, String),
    PutConcepts(String, String),
    RemoveConcepts(String),
    /// Records, `(path, id)` and `(chunk, id)` rows.
    PutCommunities(Vec<(i32, Value)>, Vec<(String, i32)>, Vec<(String, i32)>),
    ClearIndex,
}

struct PgLedgerWrite<'a> {
    pg: &'a Pg,
    ops: Vec<LedgerOp>,
}

impl LedgerWrite for PgLedgerWrite<'_> {
    fn put_file(&mut self, path: &str, entry: &FileEntry) -> crate::ledger::Result<()> {
        self.ops
            .push(LedgerOp::PutFile(path.to_string(), serde_json::to_value(entry)?));
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> crate::ledger::Result<()> {
        self.ops.push(LedgerOp::RemoveFile(path.to_string()));
        Ok(())
    }

    fn put_chunk(&mut self, chunk: &Chunk) -> crate::ledger::Result<()> {
        self.ops.push(LedgerOp::PutChunk(
            chunk.id.clone(),
            chunk.path.clone(),
            serde_json::to_value(chunk)?,
        ));
        Ok(())
    }

    fn remove_chunk(&mut self, id: &str) -> crate::ledger::Result<()> {
        self.ops.push(LedgerOp::RemoveChunk(id.to_string()));
        Ok(())
    }

    fn put_embedding(&mut self, key: &str, vector: &[f32]) -> crate::ledger::Result<()> {
        let (m, h) = split_key(key);
        self.ops.push(LedgerOp::PutEmbedding(
            m.to_string(),
            h.to_string(),
            Vector::from(vector.to_vec()),
        ));
        Ok(())
    }

    fn put_llm_cache(&mut self, model: &str, prompt_hash: &str, value: &str) -> crate::ledger::Result<()> {
        self.ops.push(LedgerOp::PutLlmCache(
            model.to_string(),
            prompt_hash.to_string(),
            value.to_string(),
        ));
        Ok(())
    }

    fn put_concepts(&mut self, path: &str, json: &str) -> crate::ledger::Result<()> {
        self.ops
            .push(LedgerOp::PutConcepts(path.to_string(), json.to_string()));
        Ok(())
    }

    fn remove_concepts(&mut self, path: &str) -> crate::ledger::Result<()> {
        self.ops.push(LedgerOp::RemoveConcepts(path.to_string()));
        Ok(())
    }

    fn set_meta(&mut self, key: &str, value: &str) -> crate::ledger::Result<()> {
        self.ops
            .push(LedgerOp::SetMeta(key.to_string(), value.to_string()));
        Ok(())
    }

    fn put_communities(
        &mut self,
        communities: &[Community],
        files: &BTreeMap<String, FileEntry>,
    ) -> crate::ledger::Result<()> {
        let mut records = Vec::new();
        let mut by_file = Vec::new();
        let mut by_chunk = Vec::new();
        for c in communities {
            records.push((c.id as i32, serde_json::to_value(c)?));
            for f in &c.files {
                by_file.push((f.clone(), c.id as i32));
                for chunk in files.get(f).map_or(&[][..], |e| e.chunk_ids.as_slice()) {
                    by_chunk.push((chunk.clone(), c.id as i32));
                }
            }
        }
        self.ops
            .push(LedgerOp::PutCommunities(records, by_file, by_chunk));
        Ok(())
    }

    fn clear_index(&mut self) -> crate::ledger::Result<()> {
        self.ops.push(LedgerOp::ClearIndex);
        Ok(())
    }

    fn commit(self: Box<Self>) -> crate::ledger::Result<()> {
        let repo = self.pg.repo;
        let mut client = self.pg.client();
        let mut tx = client.transaction().map_err(backend)?;
        for op in self.ops {
            apply_ledger(&mut tx, repo, op).map_err(backend)?;
        }
        tx.commit().map_err(backend)
    }
}

fn apply_ledger(tx: &mut Transaction<'_>, repo: i32, op: LedgerOp) -> Result<(), postgres::Error> {
    match op {
        LedgerOp::PutFile(path, entry) => tx.execute(
            "INSERT INTO rag_files (repo, path, entry) VALUES ($1, $2, $3)
             ON CONFLICT (repo, path) DO UPDATE SET entry = EXCLUDED.entry",
            &[&repo, &path, &entry],
        )?,
        LedgerOp::RemoveFile(path) => tx.execute(
            "DELETE FROM rag_files WHERE repo = $1 AND path = $2",
            &[&repo, &path],
        )?,
        LedgerOp::PutChunk(id, path, chunk) => tx.execute(
            "INSERT INTO rag_chunks (repo, id, path, chunk) VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo, id) DO UPDATE SET path = EXCLUDED.path, chunk = EXCLUDED.chunk",
            &[&repo, &id, &path, &chunk],
        )?,
        LedgerOp::RemoveChunk(id) => tx.execute(
            "DELETE FROM rag_chunks WHERE repo = $1 AND id = $2",
            &[&repo, &id],
        )?,
        LedgerOp::PutEmbedding(model, hash, vec) => tx.execute(
            "INSERT INTO rag_embeddings (model, hash, vec) VALUES ($1, $2, $3)
             ON CONFLICT (model, hash) DO NOTHING",
            &[&model, &hash, &vec],
        )?,
        LedgerOp::SetMeta(key, value) => tx.execute(
            "INSERT INTO rag_meta (repo, key, value) VALUES ($1, $2, $3)
             ON CONFLICT (repo, key) DO UPDATE SET value = EXCLUDED.value",
            &[&repo, &key, &value],
        )?,
        LedgerOp::PutLlmCache(model, hash, value) => tx.execute(
            "INSERT INTO rag_llm_cache (model, prompt_hash, value) VALUES ($1, $2, $3)
             ON CONFLICT (model, prompt_hash) DO UPDATE SET value = EXCLUDED.value",
            &[&model, &hash, &value],
        )?,
        LedgerOp::PutConcepts(path, concepts) => tx.execute(
            "INSERT INTO rag_concepts (repo, path, concepts) VALUES ($1, $2, $3)
             ON CONFLICT (repo, path) DO UPDATE SET concepts = EXCLUDED.concepts",
            &[&repo, &path, &concepts],
        )?,
        LedgerOp::RemoveConcepts(path) => tx.execute(
            "DELETE FROM rag_concepts WHERE repo = $1 AND path = $2",
            &[&repo, &path],
        )?,
        LedgerOp::PutCommunities(records, by_file, by_chunk) => {
            for table in ["rag_communities", "rag_community_files", "rag_community_chunks"] {
                tx.execute(&format!("DELETE FROM {table} WHERE repo = $1"), &[&repo])?;
            }
            for (id, record) in records {
                tx.execute(
                    "INSERT INTO rag_communities (repo, id, record) VALUES ($1, $2, $3)",
                    &[&repo, &id, &record],
                )?;
            }
            let (paths, ids): (Vec<String>, Vec<i32>) = by_file.into_iter().unzip();
            tx.execute(
                "INSERT INTO rag_community_files (repo, path, cid)
                 SELECT $1, * FROM unnest($2::text[], $3::int[])",
                &[&repo, &paths, &ids],
            )?;
            let (chunks, ids): (Vec<String>, Vec<i32>) = by_chunk.into_iter().unzip();
            tx.execute(
                "INSERT INTO rag_community_chunks (repo, chunk, cid)
                 SELECT $1, * FROM unnest($2::text[], $3::int[]) ON CONFLICT DO NOTHING",
                &[&repo, &chunks, &ids],
            )?
        }
        LedgerOp::ClearIndex => {
            tx.execute("DELETE FROM rag_files WHERE repo = $1", &[&repo])?;
            tx.execute("DELETE FROM rag_chunks WHERE repo = $1", &[&repo])?
        }
    };
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Lexical

struct PgLexical(Arc<Pg>);

impl LexicalStore for PgLexical {
    fn search(&self, query: &str, limit: usize) -> tantivy::Result<Vec<(String, f32)>> {
        let terms = code_terms(&sanitize(query));
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self
            .0
            .client()
            .query(BM25, &[&self.0.repo, &terms, &(limit as i64)])
            .map_err(lexical_err)?;
        Ok(rows.into_iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    fn writer(&self) -> tantivy::Result<Box<dyn LexicalWrite + '_>> {
        Ok(Box::new(PgLexicalWrite {
            pg: &self.0,
            ops: Vec::new(),
        }))
    }

    fn len(&self) -> u64 {
        self.0
            .client()
            .query_one("SELECT count(*) FROM rag_docs WHERE repo = $1", &[&self.0.repo])
            .map(|r| r.get::<_, i64>(0) as u64)
            .unwrap_or(0)
    }
}

/// A document: its chunk, path, field lengths and `(term, field, tf)` postings.
struct Doc {
    chunk: String,
    path: String,
    lens: [i32; 3],
    postings: Vec<(String, i16, i32)>,
}

impl Doc {
    /// The fields the Tantivy index has: text (header + content), symbols, path.
    fn of(chunk: &Chunk) -> Self {
        let fields = [chunk.indexed_text(), chunk.symbols.join(" "), chunk.path.clone()];
        let mut lens = [0i32; 3];
        let mut postings = Vec::new();
        for (f, text) in fields.iter().enumerate() {
            let terms = code_terms(text);
            lens[f] = terms.len() as i32;
            let mut tf: BTreeMap<String, i32> = BTreeMap::new();
            for t in terms {
                *tf.entry(t).or_default() += 1;
            }
            postings.extend(tf.into_iter().map(|(t, n)| (t, f as i16, n)));
        }
        Self {
            chunk: chunk.id.clone(),
            path: chunk.path.clone(),
            lens,
            postings,
        }
    }
}

enum LexicalOp {
    Add(Doc),
    RemoveFile(String),
    Clear,
}

struct PgLexicalWrite<'a> {
    pg: &'a Pg,
    ops: Vec<LexicalOp>,
}

impl LexicalWrite for PgLexicalWrite<'_> {
    fn add(&mut self, chunk: &Chunk) -> tantivy::Result<()> {
        self.ops.push(LexicalOp::Add(Doc::of(chunk)));
        Ok(())
    }

    fn remove_file(&mut self, path: &str) {
        self.ops.push(LexicalOp::RemoveFile(path.to_string()));
    }

    fn clear(&mut self) -> tantivy::Result<()> {
        self.ops.push(LexicalOp::Clear);
        Ok(())
    }

    fn commit(self: Box<Self>) -> tantivy::Result<()> {
        let repo = self.pg.repo;
        let added = self
            .ops
            .iter()
            .filter(|op| matches!(op, LexicalOp::Add(_)))
            .count();
        let mut client = self.pg.client();
        let mut tx = client.transaction().map_err(lexical_err)?;
        for op in self.ops {
            apply_lexical(&mut tx, repo, op).map_err(lexical_err)?;
        }
        tx.commit().map_err(lexical_err)?;
        if added > ANALYZE_AFTER {
            client
                .batch_execute("ANALYZE rag_docs, rag_postings, rag_chunks")
                .map_err(lexical_err)?;
        }
        Ok(())
    }
}

fn apply_lexical(tx: &mut Transaction<'_>, repo: i32, op: LexicalOp) -> Result<(), postgres::Error> {
    match op {
        LexicalOp::Add(d) => {
            tx.execute(
                "DELETE FROM rag_postings WHERE repo = $1 AND chunk = $2",
                &[&repo, &d.chunk],
            )?;
            tx.execute(
                "INSERT INTO rag_docs (repo, chunk, path, len_text, len_symbols, len_path)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (repo, chunk) DO UPDATE SET path = EXCLUDED.path,
                     len_text = EXCLUDED.len_text, len_symbols = EXCLUDED.len_symbols,
                     len_path = EXCLUDED.len_path",
                &[&repo, &d.chunk, &d.path, &d.lens[0], &d.lens[1], &d.lens[2]],
            )?;
            let mut terms = Vec::with_capacity(d.postings.len());
            let mut fields = Vec::with_capacity(d.postings.len());
            let mut tfs = Vec::with_capacity(d.postings.len());
            for (t, f, n) in d.postings {
                terms.push(t);
                fields.push(f);
                tfs.push(n);
            }
            tx.execute(
                "INSERT INTO rag_postings (repo, chunk, term, field, tf)
                 SELECT $1, $2, * FROM unnest($3::text[], $4::smallint[], $5::int[])",
                &[&repo, &d.chunk, &terms, &fields, &tfs],
            )?;
        }
        LexicalOp::RemoveFile(path) => {
            tx.execute(
                "DELETE FROM rag_postings p USING rag_docs d
                 WHERE d.repo = $1 AND d.path = $2 AND p.repo = d.repo AND p.chunk = d.chunk",
                &[&repo, &path],
            )?;
            tx.execute(
                "DELETE FROM rag_docs WHERE repo = $1 AND path = $2",
                &[&repo, &path],
            )?;
        }
        LexicalOp::Clear => {
            tx.execute("DELETE FROM rag_postings WHERE repo = $1", &[&repo])?;
            tx.execute("DELETE FROM rag_docs WHERE repo = $1", &[&repo])?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Vectors

enum VectorOp {
    Upsert(String, String, Vector),
    Remove(String),
    Clear,
}

/// The repository's vectors live in `rag_vectors`; an in-memory flat index mirrors them
/// for the scan (repository-sized), and changes reach Postgres on `persist`.
struct PgVectors {
    pg: Arc<Pg>,
    flat: FlatIndex,
    pending: Mutex<Vec<VectorOp>>,
}

impl PgVectors {
    fn push(&self, op: VectorOp) {
        self.pending.lock().expect("pending vectors lock").push(op);
    }
}

impl VectorIndex for PgVectors {
    fn upsert(&mut self, id: &str, tag: &str, vector: &[f32]) {
        self.flat.upsert(id, tag, vector);
        self.push(VectorOp::Upsert(
            id.to_string(),
            tag.to_string(),
            Vector::from(vector.to_vec()),
        ));
    }

    fn remove(&mut self, id: &str) -> bool {
        self.push(VectorOp::Remove(id.to_string()));
        self.flat.remove(id)
    }

    fn contains(&self, id: &str) -> bool {
        self.flat.contains(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(String, f32)> {
        self.flat.search(query, k)
    }

    fn len(&self) -> usize {
        self.flat.len()
    }

    fn dim(&self) -> usize {
        self.flat.dim()
    }

    fn ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        self.flat.ids()
    }

    fn tag(&self, id: &str) -> Option<&str> {
        self.flat.tag(id)
    }

    fn clear(&mut self) {
        self.flat.clear();
        self.push(VectorOp::Clear);
    }

    fn persist(&self) -> std::io::Result<()> {
        let ops = std::mem::take(&mut *self.pending.lock().expect("pending vectors lock"));
        if ops.is_empty() {
            return Ok(());
        }
        let io = |e: postgres::Error| std::io::Error::other(format!("postgres: {e}"));
        let repo = self.pg.repo;
        let mut client = self.pg.client();
        let mut tx = client.transaction().map_err(io)?;
        for op in ops {
            match op {
                VectorOp::Upsert(id, tag, vec) => tx.execute(
                    "INSERT INTO rag_vectors (repo, id, tag, vec) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (repo, id) DO UPDATE SET tag = EXCLUDED.tag, vec = EXCLUDED.vec",
                    &[&repo, &id, &tag, &vec],
                ),
                VectorOp::Remove(id) => tx.execute(
                    "DELETE FROM rag_vectors WHERE repo = $1 AND id = $2",
                    &[&repo, &id],
                ),
                VectorOp::Clear => tx.execute("DELETE FROM rag_vectors WHERE repo = $1", &[&repo]),
            }
            .map_err(io)?;
        }
        tx.commit().map_err(io)
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
    use crate::retrieve::{Mode, SearchOpts};
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

    fn open(root: &std::path::Path, storage: StorageConfig, embedder: &Arc<HashEmbedder>) -> Engine {
        let cfg = Config {
            index_dir: Some(root.join(".dendrite/rag-test")),
            storage,
            ..Config::default()
        };
        Engine::open_with_embedder(root, cfg, Some(Box::new(Shared(embedder.clone())))).unwrap()
    }

    fn remote(url: &str, repo: &str) -> StorageConfig {
        StorageConfig {
            mode: StorageMode::Remote,
            repo: Some(repo.to_string()),
            pg_url: Some(url.to_string()),
        }
    }

    fn ids(e: &Engine, q: &str, mode: Mode) -> Vec<String> {
        let opts = SearchOpts {
            top_k: 5,
            mode,
            ..SearchOpts::default()
        };
        e.search(q, &opts)
            .unwrap()
            .into_iter()
            .map(|h| h.chunk_id)
            .collect()
    }

    #[test]
    fn remote_matches_local_and_shares_embeddings() {
        let Some(url) = url() else { return };
        let dir = project();
        let local_embedder = Arc::new(HashEmbedder::new());
        let mut local = open(dir.path(), StorageConfig::default(), &local_embedder);
        local.index(false).unwrap();

        let repo = unique("parity");
        let embedder = Arc::new(HashEmbedder::new());
        let mut pg = open(dir.path(), remote(&url, &repo), &embedder);
        let r = pg.index(false).unwrap();
        assert_eq!(pg.status().unwrap().chunks, local.status().unwrap().chunks);
        // Other tests may already have cached these contents' embeddings.
        assert!(r.embedded <= r.chunks_added);

        for q in ["LocateBinary", "IndexProject", "sidecar binary", "README"] {
            assert_eq!(
                ids(&pg, q, Mode::Lexical).first(),
                ids(&local, q, Mode::Lexical).first(),
                "{q}"
            );
            assert_eq!(ids(&pg, q, Mode::Dense), ids(&local, q, Mode::Dense), "{q}");
        }

        // Reopening reads everything back; another repository with the same content reuses
        // every embedding from the shared cache.
        drop(pg);
        let again = Arc::new(HashEmbedder::new());
        let mut pg = open(dir.path(), remote(&url, &repo), &again);
        assert_eq!(pg.index(false).unwrap().files_changed, 0);
        assert_eq!(
            ids(&pg, "LocateBinary", Mode::Hybrid),
            ids(&local, "LocateBinary", Mode::Hybrid)
        );
        let other = Arc::new(HashEmbedder::new());
        let mut copy = open(dir.path(), remote(&url, &unique("copy")), &other);
        let r = copy.index(false).unwrap();
        assert!(r.chunks_added > 0);
        assert_eq!(r.embedded, 0, "embeddings are shared across repositories");
        assert_eq!(other.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn remote_incremental_changes_and_graph() {
        let Some(url) = url() else { return };
        let dir = project();
        let embedder = Arc::new(HashEmbedder::new());
        let mut pg = open(dir.path(), remote(&url, &unique("incr")), &embedder);
        pg.index(false).unwrap();
        let before = pg.status().unwrap().chunks;
        std::fs::write(
            dir.path().join("pkg/extra.go"),
            "package pkg\n\nfunc ExtraThing() {}\n",
        )
        .unwrap();
        let r = pg.index(false).unwrap();
        assert_eq!(r.files_changed, 1);
        assert!(pg.status().unwrap().chunks > before);
        assert_eq!(ids(&pg, "ExtraThing", Mode::Lexical).len(), 1);
        std::fs::remove_file(dir.path().join("pkg/extra.go")).unwrap();
        pg.index(false).unwrap();
        assert!(ids(&pg, "ExtraThing", Mode::Lexical).is_empty());
        assert!(!pg.graph_communities(10, 1).unwrap().is_empty());
    }
}
