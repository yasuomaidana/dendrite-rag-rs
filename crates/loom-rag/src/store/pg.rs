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
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{LedgerStore, LedgerWrite, LexicalStore, LexicalWrite, Store};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, LedgerError};
use crate::lexical::{code_terms, sanitize};
use crate::vector::{FlatIndex, VectorIndex};

mod ledger;
mod lexical;
mod vectors;

pub(crate) use ledger::{LedgerOp, PgLedger, apply_ledger};
use lexical::PgLexical;
pub(crate) use lexical::{LexicalOp, apply_lexical};
use vectors::PgVectors;
pub(crate) use vectors::{VectorOp, apply_vector};

/// Schema version recorded in `rag_schema`.
pub const SCHEMA_VERSION: i32 = 1;

const MIGRATION: &str = r#"
DO $$
BEGIN
    CREATE EXTENSION IF NOT EXISTS vector;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
END
$$;
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
CREATE INDEX IF NOT EXISTS rag_chunks_hash ON rag_chunks ((chunk->>'content_hash'));
CREATE TABLE IF NOT EXISTS rag_commits (
    repo int NOT NULL, sha text NOT NULL, parents text[] NOT NULL, PRIMARY KEY (repo, sha));
CREATE TABLE IF NOT EXISTS rag_branch_heads (
    repo int NOT NULL, branch text NOT NULL, sha text NOT NULL, updated_at timestamptz NOT NULL,
    PRIMARY KEY (repo, branch));
CREATE TABLE IF NOT EXISTS rag_snapshots (
    repo int NOT NULL, sha text NOT NULL, chunk text NOT NULL, path text NOT NULL, hash text NOT NULL,
    PRIMARY KEY (repo, sha, chunk));
CREATE INDEX IF NOT EXISTS rag_snapshots_hash ON rag_snapshots (hash);
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
/// text (field 0) ×1, symbols (field 1) ×2.5, path (field 2) ×1.5 ([`crate::lexical::SYMBOL_BOOST`],
/// [`crate::lexical::PATH_BOOST`]); k1 = 1.2, b = 0.75.
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
    (CASE p.field WHEN 0 THEN 1.0 WHEN 1 THEN 2.5 ELSE 1.5 END)
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

pub(crate) struct Pg {
    clients: Vec<Mutex<Client>>,
    next: AtomicUsize,
    pub(crate) repo: i32,
}

impl Pg {
    /// Connects a pool, migrates the schema and registers repository `repo`.
    pub(crate) fn connect(url: &str, repo: &str) -> Result<Self, String> {
        let pg = |e: postgres::Error| format!("postgres: {e}");
        let mut client = Client::connect(url, NoTls).map_err(pg)?;
        client.batch_execute(MIGRATION).map_err(pg)?;
        let version: Option<i32> = client
            .query_opt("SELECT version FROM rag_schema LIMIT 1", &[])
            .map_err(pg)?
            .map(|r| r.get(0));
        match version {
            None => {
                client
                    .execute("INSERT INTO rag_schema (version) VALUES ($1)", &[&SCHEMA_VERSION])
                    .map_err(pg)?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(format!(
                    "postgres schema version {v} is newer than this loom-rag ({SCHEMA_VERSION})"
                ));
            }
            Some(_) => {}
        }
        let repo_id: i32 = client
            .query_one(
                "INSERT INTO rag_repos (name) VALUES ($1)
                 ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
                &[&repo],
            )
            .map_err(pg)?
            .get(0);
        let mut clients = vec![Mutex::new(client)];
        for _ in 1..POOL {
            clients.push(Mutex::new(Client::connect(url, NoTls).map_err(pg)?));
        }
        Ok(Self {
            clients,
            next: AtomicUsize::new(0),
            repo: repo_id,
        })
    }

    /// Applies a batch of changes in one transaction.
    pub(crate) fn apply(&self, batch: SyncBatch) -> Result<(), postgres::Error> {
        let repo = self.repo;
        let mut client = self.client();
        let mut tx = client.transaction()?;
        let mut analyze = false;
        match batch {
            SyncBatch::Ledger(ops) => {
                for op in ops {
                    apply_ledger(&mut tx, repo, op)?;
                }
            }
            SyncBatch::Lexical(ops) => {
                analyze = ops.iter().filter(|op| matches!(op, LexicalOp::Add(_))).count() > ANALYZE_AFTER;
                for op in ops {
                    apply_lexical(&mut tx, repo, op)?;
                }
            }
            SyncBatch::Vectors(ops) => {
                for op in ops {
                    apply_vector(&mut tx, repo, op)?;
                }
            }
        }
        tx.commit()?;
        if analyze {
            client.batch_execute("ANALYZE rag_docs, rag_postings, rag_chunks")?;
        }
        Ok(())
    }

    /// The repository's vectors, for the in-memory scan.
    pub(crate) fn load_vectors(&self) -> Result<FlatIndex, postgres::Error> {
        let mut flat = FlatIndex::new();
        for row in self.client().query(
            "SELECT id, tag, vec FROM rag_vectors WHERE repo = $1 ORDER BY id",
            &[&self.repo],
        )? {
            let v: Vector = row.get(2);
            flat.upsert(row.get(0), row.get(1), v.as_slice());
        }
        Ok(flat)
    }

    /// A free connection if there is one, else the next in turn: concurrent readers do not
    /// queue behind each other.
    pub(crate) fn client(&self) -> MutexGuard<'_, Client> {
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
    let other = |e: String| crate::engine::Error::Other(anyhow::anyhow!(e));
    let pg = Arc::new(Pg::connect(url, repo).map_err(other)?);
    let flat = pg.load_vectors().map_err(|e| other(format!("postgres: {e}")))?;
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

/// One commit's worth of changes to one part of the store: what remote mode applies
/// directly and edge mode journals while the shared tier is unreachable.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SyncBatch {
    Ledger(Vec<LedgerOp>),
    Lexical(Vec<LexicalOp>),
    Vectors(Vec<VectorOp>),
}

/// `model ‖ 0 ‖ content_hash` (Postgres text cannot hold the NUL byte, so it is split).
fn split_key(key: &str) -> (&str, &str) {
    key.split_once('\0').unwrap_or(("", key))
}

/// Run against PostgreSQL 16 + pgvector with `cargo test --features postgres`: a container
/// started per test with testcontainers (needs Docker), or `LOOM_TEST_PG_URL` when set.
/// Without either they pass without doing anything.
#[cfg(test)]
mod tests;
