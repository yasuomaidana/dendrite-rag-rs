//! Compaction of the shared tier (`push`): what no branch reaches and nothing uses.

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::sql_types::{Double, Int4};

/// Branch heads and everything their commits reach through the parents.
const REACHABLE: &str = "WITH RECURSIVE reach(sha) AS (
            SELECT sha FROM rag_branch_heads WHERE repo = $1
            UNION
            SELECT unnest(c.parents) FROM rag_commits c JOIN reach ON c.sha = reach.sha
            WHERE c.repo = $1)";

/// Deletes the rows of `table` (keyed by `sha`) of repository `repo` that no branch head reaches.
fn delete_unreachable(conn: &mut PgConnection, repo: i32, table: &str, alias: &str) -> QueryResult<u64> {
    let sql = format!(
        "{REACHABLE} DELETE FROM {table} {alias} WHERE {alias}.repo = $1 AND {alias}.sha NOT IN (SELECT sha FROM reach)"
    );
    Ok(diesel::sql_query(sql).bind::<Int4, _>(repo).execute(conn)? as u64)
}

/// Snapshot rows of commits no branch head reaches.
pub(crate) fn unreachable_snapshots(conn: &mut PgConnection, repo: i32) -> QueryResult<u64> {
    delete_unreachable(conn, repo, "rag_snapshots", "s")
}

/// Commits no branch head reaches.
pub(crate) fn unreachable_commits(conn: &mut PgConnection, repo: i32) -> QueryResult<u64> {
    delete_unreachable(conn, repo, "rag_commits", "c")
}

/// An older version of a path whose embedding nearly equals a newer one adds nothing: drops
/// that path's snapshot rows of the old version, and the embedding once nothing else points
/// at it. Embeddings are shared, so one that a chunk of any repository, a snapshot of another
/// repository or another path's snapshot still uses is kept. Returns embeddings deleted.
pub(crate) fn near_duplicate_embeddings(
    conn: &mut PgConnection,
    repo: i32,
    distance: f64,
) -> QueryResult<u64> {
    let n = diesel::sql_query(
        "WITH old AS (
            SELECT DISTINCT e.model, e.hash, s.path
            FROM rag_snapshots s
            JOIN rag_embeddings e ON e.hash = s.hash
            WHERE s.repo = $1
              AND NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.chunk->>'content_hash' = e.hash)
              AND NOT EXISTS (SELECT 1 FROM rag_snapshots o WHERE o.hash = e.hash AND o.repo <> $1)
              AND EXISTS (
                SELECT 1 FROM rag_chunks c
                JOIN rag_embeddings n ON n.model = e.model AND n.hash = c.chunk->>'content_hash'
                WHERE c.repo = $1 AND c.path = s.path AND n.hash <> e.hash
                  AND vector_dims(n.vec) = vector_dims(e.vec)
                  AND (n.vec <=> e.vec) < $2)),
         gone AS (
            DELETE FROM rag_snapshots s USING old
            WHERE s.repo = $1 AND s.path = old.path AND s.hash = old.hash)
         DELETE FROM rag_embeddings e
         WHERE (e.model, e.hash) IN (SELECT model, hash FROM old)
           AND NOT EXISTS (
             SELECT 1 FROM rag_snapshots s
             WHERE s.hash = e.hash
               AND NOT (s.repo = $1 AND EXISTS (
                 SELECT 1 FROM old o WHERE o.path = s.path AND o.hash = s.hash)))",
    )
    .bind::<Int4, _>(repo)
    .bind::<Double, _>(distance)
    .execute(conn)?;
    Ok(n as u64)
}

/// Embeddings no current chunk and no snapshot uses.
pub(crate) fn orphaned_embeddings(conn: &mut PgConnection) -> QueryResult<u64> {
    let n = diesel::sql_query(
        "DELETE FROM rag_embeddings e
         WHERE NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.chunk->>'content_hash' = e.hash)
           AND NOT EXISTS (SELECT 1 FROM rag_snapshots s WHERE s.hash = e.hash)",
    )
    .execute(conn)?;
    Ok(n as u64)
}

/// Vector, lexical and community rows whose chunk is gone.
pub(crate) fn orphaned_rows(conn: &mut PgConnection, repo: i32) -> QueryResult<u64> {
    let mut total = 0;
    for sql in [
        "DELETE FROM rag_vectors v WHERE v.repo = $1
           AND NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.repo = v.repo AND c.id = v.id)",
        // Postings are the big table (about 200 per chunk): find the few orphan chunk ids
        // first instead of probing every posting.
        "DELETE FROM rag_postings p WHERE p.repo = $1 AND p.chunk IN (
           SELECT DISTINCT chunk FROM rag_postings WHERE repo = $1
           EXCEPT SELECT id FROM rag_chunks WHERE repo = $1)",
        "DELETE FROM rag_docs d WHERE d.repo = $1
           AND NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.repo = d.repo AND c.id = d.chunk)",
        "DELETE FROM rag_community_chunks x WHERE x.repo = $1
           AND NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.repo = x.repo AND c.id = x.chunk)",
    ] {
        total += diesel::sql_query(sql).bind::<Int4, _>(repo).execute(conn)? as u64;
    }
    Ok(total)
}
