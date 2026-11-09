//! PostgreSQL storage, the shared tier (`docs/specs/store.md` § PostgreSQL): the ledger,
//! the lexical index (BM25 computed in SQL over postings of the code tokenizer) and the
//! vectors (pgvector), for every repository of a team in one database.
//!
//! Every table is keyed by repository, except the embedding cache, which is keyed by model
//! and content hash only: an embedding computed on one machine serves every repository and
//! every machine. Writes are buffered and applied in one transaction on commit, and every
//! statement is an upsert or a keyed delete, so replaying an interrupted run is harmless.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard};

use diesel::prelude::*;
use diesel::result::Error as DbError;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use pgvector::Vector;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{LedgerStore, LedgerWrite, LexicalStore, LexicalWrite, Store};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::{FileEntry, LedgerError};
use crate::lexical::{code_terms, sanitize};
use crate::vector::{FlatIndex, VectorIndex};

mod dsl;
mod ledger;
mod lexical;
pub(crate) mod raw;
pub(crate) mod schema;
mod vectors;

pub(crate) use ledger::{LedgerOp, PgLedger, apply_ledger, present_embeddings};
use lexical::PgLexical;
pub(crate) use lexical::{LexicalOp, apply_lexical};
use schema::{rag_repos, rag_schema, rag_vectors};
use vectors::PgVectors;
pub(crate) use vectors::{VectorOp, apply_vector};

/// Schema version recorded in `rag_schema`.
pub const SCHEMA_VERSION: i32 = 1;

/// Rows per multi-row `INSERT`, under the 65,535 bind parameters of one statement.
pub(crate) const INSERT_ROWS: usize = 5_000;

/// The schema, as embedded Diesel migrations (`migrations/pg`).
const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/pg");

/// Connections shared by the ledger, lexical and vector parts of a project's store.
const POOL: usize = 4;
/// A lexical commit adding more documents than this refreshes the planner statistics.
const ANALYZE_AFTER: usize = 500;

pub(crate) struct Pg {
    conns: Vec<Mutex<PgConnection>>,
    next: AtomicUsize,
    pub(crate) repo: i32,
}

/// Migrates under an advisory lock: concurrent openers would otherwise all see an unmigrated
/// schema and race to record the same migration version.
fn migrate(conn: &mut PgConnection) -> Result<(), String> {
    const LOCK: i64 = 0x6c6f_6f6d_7261_67;
    let pg = |e: DbError| format!("postgres: {e}");
    diesel::sql_query("SELECT pg_advisory_lock($1)")
        .bind::<diesel::sql_types::BigInt, _>(LOCK)
        .execute(conn)
        .map_err(pg)?;
    let result = match raw::migrated(conn) {
        Ok(true) => Ok(()),
        Ok(false) => conn
            .run_pending_migrations(MIGRATIONS)
            .map(|_| ())
            .map_err(|e| format!("postgres: migration: {e}")),
        Err(e) => Err(pg(e)),
    };
    diesel::sql_query("SELECT pg_advisory_unlock($1)")
        .bind::<diesel::sql_types::BigInt, _>(LOCK)
        .execute(conn)
        .map_err(pg)?;
    result
}

impl Pg {
    /// Connects a pool, migrates the schema and registers repository `repo`.
    pub(crate) fn connect(url: &str, repo: &str) -> Result<Self, String> {
        let pg = |e: DbError| format!("postgres: {e}");
        let open = || PgConnection::establish(url).map_err(|e| format!("postgres: {e}"));
        let mut conn = open()?;
        // The migration is one transaction, so the last table's presence means the schema is
        // complete; re-running it is skipped (see `raw::migrated`).
        migrate(&mut conn)?;
        let version: Option<i32> = rag_schema::table
            .select(rag_schema::version)
            .first(&mut conn)
            .optional()
            .map_err(pg)?;
        match version {
            None => {
                diesel::insert_into(rag_schema::table)
                    .values(rag_schema::version.eq(SCHEMA_VERSION))
                    .execute(&mut conn)
                    .map_err(pg)?;
            }
            Some(v) if v > SCHEMA_VERSION => {
                return Err(format!(
                    "postgres schema version {v} is newer than this loom-rag ({SCHEMA_VERSION})"
                ));
            }
            Some(_) => {}
        }
        let repo_id: i32 = diesel::insert_into(rag_repos::table)
            .values(rag_repos::name.eq(repo))
            .on_conflict(rag_repos::name)
            .do_update()
            .set(rag_repos::name.eq(diesel::upsert::excluded(rag_repos::name)))
            .returning(rag_repos::id)
            .get_result(&mut conn)
            .map_err(pg)?;
        let mut conns = vec![Mutex::new(conn)];
        for _ in 1..POOL {
            conns.push(Mutex::new(open()?));
        }
        Ok(Self {
            conns,
            next: AtomicUsize::new(0),
            repo: repo_id,
        })
    }

    /// Applies a batch of changes in one transaction.
    pub(crate) fn apply(&self, batch: SyncBatch) -> Result<(), DbError> {
        let repo = self.repo;
        let mut conn = self.conn();
        let analyze = conn.transaction::<_, DbError, _>(|tx| apply_batch(tx, repo, batch))?;
        if analyze {
            raw::analyze(&mut conn)?;
        }
        Ok(())
    }

    /// The repository's vectors, for the in-memory scan.
    pub(crate) fn load_vectors(&self) -> Result<FlatIndex, DbError> {
        let mut flat = FlatIndex::new();
        let rows: Vec<(String, String, Vector)> = rag_vectors::table
            .filter(rag_vectors::repo.eq(self.repo))
            .order(rag_vectors::id)
            .select((rag_vectors::id, rag_vectors::tag, rag_vectors::vec))
            .load(&mut *self.conn())?;
        for (id, tag, v) in rows {
            flat.upsert(&id, &tag, v.as_slice());
        }
        Ok(flat)
    }

    /// Runs a query on a free connection; a failure becomes a readable backend error.
    pub(crate) fn run<T>(
        &self,
        f: impl FnOnce(&mut PgConnection) -> QueryResult<T>,
    ) -> Result<T, LedgerError> {
        f(&mut self.conn()).map_err(backend)
    }

    /// A free connection if there is one, else the next in turn.
    pub(crate) fn conn(&self) -> MutexGuard<'_, PgConnection> {
        super::pick_connection(&self.conns, &self.next, "postgres connection")
    }
}

/// Applies `batch` inside `tx`; whether the planner statistics need refreshing afterwards.
fn apply_batch(tx: &mut PgConnection, repo: i32, batch: SyncBatch) -> Result<bool, DbError> {
    match batch {
        SyncBatch::Ledger(ops) => ops.into_iter().try_for_each(|op| apply_ledger(tx, repo, op))?,
        SyncBatch::Vectors(ops) => ops.into_iter().try_for_each(|op| apply_vector(tx, repo, op))?,
        SyncBatch::Lexical(ops) => {
            let adds = ops.iter().filter(|op| matches!(op, LexicalOp::Add(_))).count();
            ops.into_iter().try_for_each(|op| apply_lexical(tx, repo, op))?;
            return Ok(adds > ANALYZE_AFTER);
        }
    }
    Ok(false)
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
