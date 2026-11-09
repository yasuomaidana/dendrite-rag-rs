//! The only module that writes SQL by hand (`diesel::sql_query`): the statements the Diesel
//! DSL cannot express. Every other store module goes through the DSL, and clippy rejects
//! `sql_query` outside this module (`clippy.toml`).
#![allow(clippy::disallowed_methods)]

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::sql_types::{Array, BigInt, Bool, Double, Float4, Int4, Text};

use crate::bm25::Bm25Params;

pub(crate) mod compact;

/// The last table the init migration creates. The migration is one transaction, so its
/// presence means the schema is complete and re-running it is skipped: `CREATE INDEX IF NOT
/// EXISTS` still locks its table, and that lock deadlocked with another connection's writes.
const MIGRATED_TABLE: &str = "rag_concepts";

/// Whether the schema is already in place (also for databases created before Diesel).
pub(crate) fn migrated(conn: &mut PgConnection) -> QueryResult<bool> {
    #[derive(QueryableByName)]
    struct Present {
        #[diesel(sql_type = Bool)]
        present: bool,
    }
    Ok(diesel::sql_query("SELECT to_regclass($1) IS NOT NULL AS present")
        .bind::<Text, _>(MIGRATED_TABLE)
        .get_result::<Present>(conn)?
        .present)
}

/// Advisory-lock key serializing migrations across connections.
const MIGRATION_LOCK: i64 = 0x006c_6f6f_6d72_6167;

/// Blocks until this session holds the migration lock.
pub(crate) fn lock_migrations(conn: &mut PgConnection) -> QueryResult<()> {
    diesel::sql_query("SELECT pg_advisory_lock($1)")
        .bind::<BigInt, _>(MIGRATION_LOCK)
        .execute(conn)?;
    Ok(())
}

/// Releases the migration lock.
pub(crate) fn unlock_migrations(conn: &mut PgConnection) -> QueryResult<()> {
    diesel::sql_query("SELECT pg_advisory_unlock($1)")
        .bind::<BigInt, _>(MIGRATION_LOCK)
        .execute(conn)?;
    Ok(())
}

/// Refreshes the planner statistics the BM25 query plan depends on.
pub(crate) fn analyze(conn: &mut PgConnection) -> QueryResult<()> {
    diesel::sql_query("ANALYZE rag_docs, rag_postings, rag_chunks").execute(conn)?;
    Ok(())
}

/// Okapi BM25 over the postings, the same formula and field boosts as the Tantivy index.
/// `stats` must stay MATERIALIZED: inlined, the planner re-aggregates it for every posting
/// (500 ms instead of 5 ms on 5k chunks).
///
/// text (field 0) ×1, symbols (field 1) ×`$7`, path (field 2) ×`$8` ([`crate::lexical::SYMBOL_BOOST`],
/// [`crate::lexical::PATH_BOOST`]); `$4` k1, `$5` b and `$6` q of [`crate::bm25::Bm25Params`],
/// the same formula as [`crate::bm25`].
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
    (CASE p.field WHEN 0 THEN 1.0 WHEN 1 THEN $7::float8 ELSE $8::float8 END)
    * (CASE WHEN $6::float8 = 1 THEN ln(1 + (s.n - df.df + 0.5) / (df.df + 0.5))
       ELSE (power(1 + (s.n - df.df + 0.5) / (df.df + 0.5), 1 - $6::float8) - 1) / (1 - $6::float8) END)
    * (p.tf * ($4::float8 + 1)) / (p.tf + $4::float8 * ((1 - $5::float8) + $5::float8 * (CASE p.field
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

#[derive(QueryableByName)]
struct Scored {
    #[diesel(sql_type = Text)]
    chunk: String,
    #[diesel(sql_type = Float4)]
    score: f32,
}

/// The `limit` best chunks of repository `repo` for `terms`, best first.
pub(crate) fn bm25(
    conn: &mut PgConnection,
    repo: i32,
    terms: &[String],
    limit: usize,
    params: &Bm25Params,
) -> QueryResult<Vec<(String, f32)>> {
    let rows = diesel::sql_query(BM25)
        .bind::<Int4, _>(repo)
        .bind::<Array<Text>, _>(terms)
        .bind::<BigInt, _>(limit as i64)
        .bind::<Double, _>(f64::from(params.k1))
        .bind::<Double, _>(f64::from(params.b))
        .bind::<Double, _>(f64::from(params.q))
        .bind::<Double, _>(f64::from(crate::lexical::SYMBOL_BOOST))
        .bind::<Double, _>(f64::from(crate::lexical::PATH_BOOST))
        .load::<Scored>(conn)?;
    Ok(rows.into_iter().map(|r| (r.chunk, r.score)).collect())
}
