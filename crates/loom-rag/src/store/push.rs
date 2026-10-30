//! `push` (`docs/specs/store.md` § Push and compaction): uploads a project's local index to
//! the shared tier, records which chunks the current commit holds, then compacts the tier.
//! Everything runs in one transaction, so `--dry-run` executes the same statements and rolls
//! them back: its report is exactly what a real push would do.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use postgres::Transaction;
use serde::{Deserialize, Serialize};

use super::pg::{LedgerOp, LexicalOp, Pg, VectorOp, apply_ledger, apply_lexical, apply_vector};
use crate::chunk::Chunk;
use crate::graph::communities::Community;
use crate::ledger::FileEntry;

/// Two embeddings of versions of the same path this close (cosine distance) are duplicates.
pub const NEAR_DUPLICATE_DISTANCE: f64 = 0.005;
/// Commits walked back from HEAD when recording parents.
const MAX_COMMITS: usize = 5000;

/// The git state a push records.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GitState {
    pub head: String,
    pub branch: String,
    /// `(sha, parents)`, HEAD first.
    pub commits: Vec<(String, Vec<String>)>,
    /// `(branch, sha)` for every local branch and every `origin` remote-tracking branch.
    pub branches: Vec<(String, String)>,
}

impl GitState {
    /// Reads HEAD, the current branch, its ancestry and the local branch heads.
    pub fn read(root: &Path) -> Result<Self, String> {
        let git = |args: &[&str]| -> Result<String, String> {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .map_err(|e| format!("git: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "git {}: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        let head = git(&["rev-parse", "HEAD"])?;
        let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        let commits = git(&[
            "rev-list",
            "--parents",
            &format!("--max-count={MAX_COMMITS}"),
            "HEAD",
        ])?
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace().map(str::to_string);
            Some((parts.next()?, parts.collect()))
        })
        .collect();
        let branches = git(&[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads",
            "refs/remotes/origin",
        ])?
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(b, s)| (b.to_string(), s.to_string())))
        .collect();
        Ok(Self {
            head,
            branch,
            commits,
            branches,
        })
    }
}

/// The local index content a push uploads.
pub struct LocalIndex {
    pub files: BTreeMap<String, FileEntry>,
    pub chunks: Vec<Chunk>,
    /// `(model, content_hash, vector)` for the current model.
    pub embeddings: Vec<(String, String, Vec<f32>)>,
    /// `(chunk_id, content_hash)` of every vector.
    pub vectors: Vec<(String, String)>,
    pub communities: Vec<Community>,
    pub meta: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PushReport {
    pub dry_run: bool,
    pub commit: String,
    pub branch: String,
    pub files: usize,
    pub chunks: usize,
    /// Embeddings the shared tier did not have yet.
    pub embeddings_uploaded: usize,
    pub vectors: usize,
    pub compaction: CompactionReport,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompactionReport {
    /// Snapshot rows of commits no branch head reaches.
    pub snapshots_unreachable: u64,
    pub commits_unreachable: u64,
    /// Embeddings no current chunk or snapshot uses.
    pub embeddings_orphaned: u64,
    /// Embeddings of older versions of a path, cosine > 0.995 to a newer one.
    pub embeddings_near_duplicate: u64,
    /// Lexical, vector and community rows whose chunk is gone.
    pub rows_orphaned: u64,
}

pub(crate) fn push(
    pg: &Pg,
    local: LocalIndex,
    git: &GitState,
    dry_run: bool,
) -> Result<PushReport, postgres::Error> {
    let repo = pg.repo;
    let mut client = pg.client();
    let mut tx = client.transaction()?;
    let mut report = PushReport {
        dry_run,
        commit: git.head.clone(),
        branch: git.branch.clone(),
        files: local.files.len(),
        chunks: local.chunks.len(),
        vectors: local.vectors.len(),
        ..PushReport::default()
    };
    upload(&mut tx, repo, &local, &mut report)?;
    record(&mut tx, repo, &local, git)?;
    report.compaction = compact(&mut tx, repo)?;
    if dry_run {
        tx.rollback()?;
    } else {
        tx.commit()?;
    }
    Ok(report)
}

/// Replaces the repository's current state in the tier with the local index; embeddings
/// already in the tier are not sent again.
fn upload(
    tx: &mut Transaction<'_>,
    repo: i32,
    local: &LocalIndex,
    report: &mut PushReport,
) -> Result<(), postgres::Error> {
    apply_ledger(tx, repo, LedgerOp::ClearIndex)?;
    for (path, entry) in &local.files {
        let entry = serde_json::to_value(entry).expect("file entries serialize");
        apply_ledger(tx, repo, LedgerOp::PutFile(path.clone(), entry))?;
    }
    for c in &local.chunks {
        let value = serde_json::to_value(c).expect("chunks serialize");
        apply_ledger(tx, repo, LedgerOp::PutChunk(c.id.clone(), c.path.clone(), value))?;
    }
    let (models, hashes): (Vec<&str>, Vec<&str>) = local
        .embeddings
        .iter()
        .map(|(m, h, _)| (m.as_str(), h.as_str()))
        .unzip();
    let present: HashSet<(String, String)> = tx
        .query(
            "SELECT e.model, e.hash FROM unnest($1::text[], $2::text[]) AS k(model, hash)
             JOIN rag_embeddings e ON e.model = k.model AND e.hash = k.hash",
            &[&models, &hashes],
        )?
        .into_iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    for (model, hash, vec) in &local.embeddings {
        if !present.contains(&(model.clone(), hash.clone())) {
            apply_ledger(
                tx,
                repo,
                LedgerOp::PutEmbedding(model.clone(), hash.clone(), vec.clone()),
            )?;
            report.embeddings_uploaded += 1;
        }
    }
    for (k, v) in &local.meta {
        apply_ledger(tx, repo, LedgerOp::SetMeta(k.clone(), v.clone()))?;
    }
    let communities = LedgerOp::communities(&local.communities, &local.files).expect("communities serialize");
    apply_ledger(tx, repo, communities)?;

    apply_lexical(tx, repo, LexicalOp::Clear)?;
    for c in &local.chunks {
        apply_lexical(tx, repo, LexicalOp::Add(Box::new(c.clone())))?;
    }
    apply_vector(tx, repo, VectorOp::Clear)?;
    let by_hash: BTreeMap<&str, &Vec<f32>> =
        local.embeddings.iter().map(|(_, h, v)| (h.as_str(), v)).collect();
    for (id, hash) in &local.vectors {
        if let Some(v) = by_hash.get(hash.as_str()) {
            apply_vector(tx, repo, VectorOp::Upsert(id.clone(), hash.clone(), (*v).clone()))?;
        }
    }
    Ok(())
}

/// Records the commit graph, the branch heads and the chunks HEAD holds.
fn record(
    tx: &mut Transaction<'_>,
    repo: i32,
    local: &LocalIndex,
    git: &GitState,
) -> Result<(), postgres::Error> {
    for (sha, parents) in &git.commits {
        tx.execute(
            "INSERT INTO rag_commits (repo, sha, parents) VALUES ($1, $2, $3)
             ON CONFLICT (repo, sha) DO NOTHING",
            &[&repo, sha, parents],
        )?;
    }
    // The pushing clone's branches (its own and origin's) are the tracked heads; a branch
    // deleted everywhere stops keeping its commits alive.
    let names: Vec<&str> = git.branches.iter().map(|(b, _)| b.as_str()).collect();
    tx.execute(
        "DELETE FROM rag_branch_heads WHERE repo = $1 AND branch <> ALL($2)",
        &[&repo, &names],
    )?;
    for (branch, sha) in &git.branches {
        tx.execute(
            "INSERT INTO rag_branch_heads (repo, branch, sha, updated_at) VALUES ($1, $2, $3, now())
             ON CONFLICT (repo, branch) DO UPDATE SET sha = EXCLUDED.sha, updated_at = now()",
            &[&repo, branch, sha],
        )?;
    }
    tx.execute(
        "DELETE FROM rag_snapshots WHERE repo = $1 AND sha = $2",
        &[&repo, &git.head],
    )?;
    let (ids, rest): (Vec<&str>, Vec<(&str, &str)>) = local
        .chunks
        .iter()
        .map(|c| (c.id.as_str(), (c.path.as_str(), c.content_hash.as_str())))
        .unzip();
    let (paths, hashes): (Vec<&str>, Vec<&str>) = rest.into_iter().unzip();
    tx.execute(
        "INSERT INTO rag_snapshots (repo, sha, chunk, path, hash)
         SELECT $1, $2, * FROM unnest($3::text[], $4::text[], $5::text[])
         ON CONFLICT DO NOTHING",
        &[&repo, &git.head, &ids, &paths, &hashes],
    )?;
    Ok(())
}

/// Removes what no branch can reach and what nothing uses.
fn compact(tx: &mut Transaction<'_>, repo: i32) -> Result<CompactionReport, postgres::Error> {
    let mut r = CompactionReport::default();
    let reachable = "WITH RECURSIVE reach(sha) AS (
            SELECT sha FROM rag_branch_heads WHERE repo = $1
            UNION
            SELECT unnest(c.parents) FROM rag_commits c JOIN reach ON c.sha = reach.sha
            WHERE c.repo = $1)";
    r.snapshots_unreachable = tx.execute(
        &format!("{reachable} DELETE FROM rag_snapshots s WHERE s.repo = $1 AND s.sha NOT IN (SELECT sha FROM reach)"),
        &[&repo],
    )?;
    r.commits_unreachable = tx.execute(
        &format!(
            "{reachable} DELETE FROM rag_commits c WHERE c.repo = $1 AND c.sha NOT IN (SELECT sha FROM reach)"
        ),
        &[&repo],
    )?;
    // An older version of a path whose embedding nearly equals a newer one adds nothing:
    // drop the embedding when only snapshots use it, and those snapshot rows with it.
    r.embeddings_near_duplicate = tx.execute(
        "WITH old AS (
            SELECT DISTINCT e.model, e.hash
            FROM rag_snapshots s
            JOIN rag_embeddings e ON e.hash = s.hash
            WHERE s.repo = $1
              AND NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.chunk->>'content_hash' = e.hash)
              AND EXISTS (
                SELECT 1 FROM rag_chunks c
                JOIN rag_embeddings n ON n.model = e.model AND n.hash = c.chunk->>'content_hash'
                WHERE c.repo = $1 AND c.path = s.path AND n.hash <> e.hash
                  AND vector_dims(n.vec) = vector_dims(e.vec)
                  AND (n.vec <=> e.vec) < $2))
         DELETE FROM rag_embeddings e USING old WHERE e.model = old.model AND e.hash = old.hash",
        &[&repo, &NEAR_DUPLICATE_DISTANCE],
    )?;
    r.embeddings_orphaned = tx.execute(
        "DELETE FROM rag_embeddings e
         WHERE NOT EXISTS (SELECT 1 FROM rag_chunks c WHERE c.chunk->>'content_hash' = e.hash)
           AND NOT EXISTS (SELECT 1 FROM rag_snapshots s WHERE s.hash = e.hash)",
        &[],
    )?;
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
        r.rows_orphaned += tx.execute(sql, &[&repo])?;
    }
    Ok(r)
}

/// Run against PostgreSQL 16 + pgvector with `cargo test --features postgres`: a container
/// started per test with testcontainers (needs Docker), or `LOOM_TEST_PG_URL` when set.
/// Without either they pass without doing anything.
#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::engine::Engine;
    use crate::store::StorageMode;
    use crate::testutil::project;

    fn git(root: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn engine(root: &Path, url: &str, repo: &str, mode: StorageMode, index: &str) -> Engine {
        crate::testutil::engine_with(root, index, crate::testutil::pg_storage(mode, url, repo), None)
    }

    fn words(n: usize, extra: &str) -> String {
        let body: Vec<String> = (0..n).map(|i| format!("word{i}")).collect();
        format!("{}{extra}\n", body.join(" "))
    }

    #[test]
    fn push_uploads_records_and_compacts() {
        let Some(pg) = crate::testutil::test_pg() else {
            return;
        };
        let url = pg.url.clone();
        let dir = project();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), ".loom/\n").unwrap();
        // Unique text per run: embeddings are shared across repositories, so content left
        // by an earlier run in the same database would still be in use.
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        let notes = |extra: &str| words(300, &format!(" run{tag}{extra}"));
        std::fs::write(root.join("notes.txt"), notes("")).unwrap();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "a"]);
        let repo = format!(
            "push-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut local = engine(root, &url, &repo, StorageMode::Local, ".loom");
        local.index(false).unwrap();

        // A dry run reports and changes nothing.
        let dry = local.push(true).unwrap();
        assert!(dry.dry_run && dry.chunks > 0);
        let remote = engine(root, &url, &repo, StorageMode::Remote, ".loom-remote");
        assert_eq!(remote.status().unwrap().chunks, 0);
        drop(remote);

        let real = local.push(false).unwrap();
        assert_eq!((real.files, real.chunks), (dry.files, dry.chunks));
        assert_eq!(real.compaction, dry.compaction);
        let remote = engine(root, &url, &repo, StorageMode::Remote, ".loom-remote");
        assert_eq!(remote.status().unwrap().chunks, local.status().unwrap().chunks);
        drop(remote);

        // A branch with a nearly identical version of notes.txt, then a different one on
        // main: once the branch is deleted, its commit is unreachable and compacted away.
        git(root, &["checkout", "-qb", "tmp"]);
        std::fs::write(root.join("notes.txt"), notes(" extra")).unwrap();
        git(root, &["commit", "-qam", "tmp"]);
        local.index(false).unwrap();
        local.push(false).unwrap();
        git(root, &["checkout", "-q", "main"]);
        git(root, &["branch", "-qD", "tmp"]);
        local.index(false).unwrap();
        let after = local.push(false).unwrap();
        assert!(after.compaction.snapshots_unreachable > 0, "{after:?}");
        assert_eq!(after.compaction.commits_unreachable, 1, "{after:?}");
        assert!(
            after.compaction.embeddings_orphaned + after.compaction.embeddings_near_duplicate > 0,
            "the tmp version's embedding is gone: {after:?}"
        );
        // Nothing left to compact on a second push.
        let again = local.push(true).unwrap();
        assert_eq!(again.compaction, crate::store::push::CompactionReport::default());
    }
}
