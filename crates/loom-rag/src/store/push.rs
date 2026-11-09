//! `push` (`docs/specs/store.md` § Push and compaction): uploads a project's local index to
//! the shared tier, records which chunks the current commit holds, then compacts the tier.
//! Everything runs in one transaction, so `--dry-run` executes the same statements and rolls
//! them back: its report is exactly what a real push would do.

use std::collections::BTreeMap;
use std::path::Path;

use diesel::prelude::*;
use diesel::result::Error as DbError;
use diesel::upsert::excluded;
use serde::{Deserialize, Serialize};

use super::pg::schema::{rag_branch_heads, rag_commits, rag_snapshots};
use super::pg::{
    INSERT_ROWS, LedgerOp, LexicalOp, Pg, VectorOp, apply_ledger, apply_lexical, apply_vector,
    present_embeddings, raw::compact as sql,
};
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

pub(crate) fn push(pg: &Pg, local: LocalIndex, git: &GitState, dry_run: bool) -> Result<PushReport, DbError> {
    let repo = pg.repo;
    let mut conn = pg.conn();
    let mut report = PushReport {
        dry_run,
        commit: git.head.clone(),
        branch: git.branch.clone(),
        files: local.files.len(),
        chunks: local.chunks.len(),
        vectors: local.vectors.len(),
        ..PushReport::default()
    };
    let done = conn.transaction::<_, DbError, _>(|tx| {
        upload(tx, repo, &local, &mut report)?;
        record(tx, repo, &local, git)?;
        report.compaction = compact(tx, repo)?;
        // A dry run rolls everything back, so its report is what a real push does.
        if dry_run {
            Err(DbError::RollbackTransaction)
        } else {
            Ok(())
        }
    });
    match done {
        Ok(()) | Err(DbError::RollbackTransaction) => Ok(report),
        Err(e) => Err(e),
    }
}

/// Replaces the repository's current state in the tier with the local index; embeddings
/// already in the tier are not sent again.
fn upload(
    tx: &mut PgConnection,
    repo: i32,
    local: &LocalIndex,
    report: &mut PushReport,
) -> Result<(), DbError> {
    apply_ledger(tx, repo, LedgerOp::ClearIndex)?;
    for (path, entry) in &local.files {
        let entry = serde_json::to_value(entry).expect("file entries serialize");
        apply_ledger(tx, repo, LedgerOp::PutFile(path.clone(), entry))?;
    }
    for c in &local.chunks {
        let value = serde_json::to_value(c).expect("chunks serialize");
        apply_ledger(tx, repo, LedgerOp::PutChunk(c.id.clone(), c.path.clone(), value))?;
    }
    let keys: Vec<(String, String)> = local
        .embeddings
        .iter()
        .map(|(m, h, _)| (m.clone(), h.clone()))
        .collect();
    let present = present_embeddings(tx, &keys)?;
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

#[derive(Insertable)]
#[diesel(table_name = rag_commits)]
struct NewCommit<'a> {
    repo: i32,
    sha: &'a str,
    parents: &'a [String],
}

#[derive(Insertable)]
#[diesel(table_name = rag_snapshots)]
struct NewSnapshot<'a> {
    repo: i32,
    sha: &'a str,
    chunk: &'a str,
    path: &'a str,
    hash: &'a str,
}

/// Records the commit graph, the branch heads and the chunks HEAD holds.
fn record(tx: &mut PgConnection, repo: i32, local: &LocalIndex, git: &GitState) -> Result<(), DbError> {
    let commits: Vec<NewCommit<'_>> = git
        .commits
        .iter()
        .map(|(sha, parents)| NewCommit { repo, sha, parents })
        .collect();
    for rows in commits.chunks(INSERT_ROWS) {
        diesel::insert_into(rag_commits::table)
            .values(rows)
            .on_conflict_do_nothing()
            .execute(tx)?;
    }
    // The pushing clone's branches (its own and origin's) are the tracked heads; a branch
    // deleted everywhere stops keeping its commits alive.
    let names: Vec<&str> = git.branches.iter().map(|(b, _)| b.as_str()).collect();
    diesel::delete(
        rag_branch_heads::table
            .filter(rag_branch_heads::repo.eq(repo))
            .filter(diesel::dsl::not(rag_branch_heads::branch.eq_any(names))),
    )
    .execute(tx)?;
    for (branch, sha) in &git.branches {
        diesel::insert_into(rag_branch_heads::table)
            .values((
                rag_branch_heads::repo.eq(repo),
                rag_branch_heads::branch.eq(branch),
                rag_branch_heads::sha.eq(sha),
                rag_branch_heads::updated_at.eq(diesel::dsl::now),
            ))
            .on_conflict((rag_branch_heads::repo, rag_branch_heads::branch))
            .do_update()
            .set((
                rag_branch_heads::sha.eq(excluded(rag_branch_heads::sha)),
                rag_branch_heads::updated_at.eq(diesel::dsl::now),
            ))
            .execute(tx)?;
    }
    diesel::delete(
        rag_snapshots::table.filter(rag_snapshots::repo.eq(repo).and(rag_snapshots::sha.eq(&git.head))),
    )
    .execute(tx)?;
    let snapshots: Vec<NewSnapshot<'_>> = local
        .chunks
        .iter()
        .map(|c| NewSnapshot {
            repo,
            sha: &git.head,
            chunk: &c.id,
            path: &c.path,
            hash: &c.content_hash,
        })
        .collect();
    for rows in snapshots.chunks(INSERT_ROWS) {
        diesel::insert_into(rag_snapshots::table)
            .values(rows)
            .on_conflict_do_nothing()
            .execute(tx)?;
    }
    Ok(())
}

/// Removes what no branch can reach and what nothing uses.
fn compact(tx: &mut PgConnection, repo: i32) -> Result<CompactionReport, DbError> {
    Ok(CompactionReport {
        snapshots_unreachable: sql::unreachable_snapshots(tx, repo)?,
        commits_unreachable: sql::unreachable_commits(tx, repo)?,
        embeddings_near_duplicate: sql::near_duplicate_embeddings(tx, repo, NEAR_DUPLICATE_DISTANCE)?,
        embeddings_orphaned: sql::orphaned_embeddings(tx)?,
        rows_orphaned: sql::orphaned_rows(tx, repo)?,
    })
}

/// Run against PostgreSQL 16 + pgvector with `cargo test --features postgres`: a container
/// started per test with testcontainers (needs Docker), or `LOOM_TEST_PG_URL` when set.
/// Without either they pass without doing anything.
#[cfg(test)]
mod tests {
    use std::path::Path;

    use diesel::dsl::{exists, not};
    use diesel::prelude::*;

    use crate::engine::Engine;
    use crate::store::StorageMode;
    use crate::store::pg::schema::{rag_chunks, rag_commits, rag_embeddings, rag_repos, rag_snapshots};
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

    /// Compaction in one repository must not delete an embedding that a snapshot of another
    /// repository still points at.
    #[test]
    fn compaction_keeps_embeddings_other_repos_snapshots_use() {
        let Some(pg) = crate::testutil::test_pg() else {
            return;
        };
        let url = pg.url.clone();
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        let v1 = words(300, &format!(" shared{tag}"));
        let make = |content: &str| {
            let dir = project();
            std::fs::write(dir.path().join(".gitignore"), ".loom/\n").unwrap();
            std::fs::write(dir.path().join("notes.txt"), content).unwrap();
            git(dir.path(), &["init", "-q", "-b", "main"]);
            git(dir.path(), &["add", "-A"]);
            git(dir.path(), &["commit", "-qm", "v1"]);
            dir
        };
        // Repository B keeps v1 only in a snapshot: its head moved on to unrelated text.
        let b_dir = make(&v1);
        let b_repo = format!("b-{tag}");
        let mut b = engine(b_dir.path(), &url, &b_repo, StorageMode::Local, ".loom");
        b.index(false).unwrap();
        b.push(false).unwrap();
        let other: Vec<String> = (0..300).map(|i| format!("other{tag}x{i}")).collect();
        std::fs::write(b_dir.path().join("notes.txt"), other.join(" ")).unwrap();
        git(b_dir.path(), &["commit", "-qam", "v2"]);
        b.index(false).unwrap();
        b.push(false).unwrap();

        // Repository A pushes v1, then a nearly identical v2: v1 is an old near-duplicate there.
        let a_dir = make(&v1);
        let a_repo = format!("a-{tag}");
        let mut a = engine(a_dir.path(), &url, &a_repo, StorageMode::Local, ".loom");
        a.index(false).unwrap();
        a.push(false).unwrap();
        std::fs::write(a_dir.path().join("notes.txt"), format!("{v1} extra")).unwrap();
        git(a_dir.path(), &["commit", "-qam", "v2"]);
        a.index(false).unwrap();
        a.push(false).unwrap();

        let mut conn = PgConnection::establish(&url).unwrap();
        let dangling: i64 = rag_snapshots::table
            .inner_join(rag_repos::table.on(rag_repos::id.eq(rag_snapshots::repo)))
            .filter(rag_repos::name.eq(&b_repo))
            .filter(not(exists(
                rag_embeddings::table.filter(rag_embeddings::hash.eq(rag_snapshots::hash)),
            )))
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(dangling, 0, "repository B's snapshots lost their embeddings");
    }

    /// Only the path whose newer version nearly equals the old one loses its snapshot row;
    /// another path that points at the same old content in the same commit keeps it, and so
    /// does the embedding it uses.
    #[test]
    fn compaction_drops_only_the_superseded_paths_snapshot() {
        let Some(pg) = crate::testutil::test_pg() else {
            return;
        };
        let url = pg.url.clone();
        let tag = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        let v1 = words(300, &format!(" twopaths{tag}"));
        let dir = project();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), ".loom/\n").unwrap();
        std::fs::write(root.join("p1.txt"), &v1).unwrap();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "v1"]);
        let repo = format!("paths-{tag}");
        let mut e = engine(root, &url, &repo, StorageMode::Local, ".loom");
        e.index(false).unwrap();
        e.push(false).unwrap();
        std::fs::write(root.join("p1.txt"), format!("{v1} extra")).unwrap();
        git(root, &["commit", "-qam", "v2"]);
        e.index(false).unwrap();
        e.push(false).unwrap();

        // An old version of p1.txt whose embedding equals the current p1.txt embedding (the
        // test embedder does not place near-identical text close together), and the same old
        // content at p2.txt in that commit.
        let mut conn = PgConnection::establish(&url).unwrap();
        let old_hash = format!("old{tag}");
        let repo_id: i32 = rag_repos::table
            .filter(rag_repos::name.eq(&repo))
            .select(rag_repos::id)
            .first(&mut conn)
            .unwrap();
        let chunk: serde_json::Value = rag_chunks::table
            .filter(rag_chunks::repo.eq(repo_id).and(rag_chunks::path.eq("p1.txt")))
            .select(rag_chunks::chunk)
            .first(&mut conn)
            .unwrap();
        let current_hash = chunk["content_hash"].as_str().unwrap().to_string();
        let (model, vec): (String, pgvector::Vector) = rag_embeddings::table
            .filter(rag_embeddings::hash.eq(&current_hash))
            .select((rag_embeddings::model, rag_embeddings::vec))
            .first(&mut conn)
            .unwrap();
        diesel::insert_into(rag_embeddings::table)
            .values((
                rag_embeddings::model.eq(model),
                rag_embeddings::hash.eq(&old_hash),
                rag_embeddings::vec.eq(vec),
            ))
            .execute(&mut conn)
            .unwrap();
        let roots: Vec<String> = rag_commits::table
            .filter(rag_commits::repo.eq(repo_id))
            .select((rag_commits::sha, rag_commits::parents))
            .load::<(String, Vec<String>)>(&mut conn)
            .unwrap()
            .into_iter()
            .filter(|(_, parents)| parents.is_empty())
            .map(|(sha, _)| sha)
            .collect();
        let mut rows = 0;
        for sha in &roots {
            for (chunk, path) in [("old-a", "p1.txt"), ("old-b", "p2.txt")] {
                rows += diesel::insert_into(rag_snapshots::table)
                    .values((
                        rag_snapshots::repo.eq(repo_id),
                        rag_snapshots::sha.eq(sha),
                        rag_snapshots::chunk.eq(chunk),
                        rag_snapshots::path.eq(path),
                        rag_snapshots::hash.eq(&old_hash),
                    ))
                    .execute(&mut conn)
                    .unwrap();
            }
        }
        assert_eq!(rows, 2);
        e.push(false).unwrap();

        let snapshot = |conn: &mut PgConnection, chunk: &str| -> i64 {
            rag_snapshots::table
                .filter(
                    rag_snapshots::repo
                        .eq(repo_id)
                        .and(rag_snapshots::chunk.eq(chunk)),
                )
                .count()
                .get_result(conn)
                .unwrap()
        };
        assert_eq!(
            snapshot(&mut conn, "old-a"),
            0,
            "the superseded p1.txt row is dropped"
        );
        assert_eq!(snapshot(&mut conn, "old-b"), 1, "p2.txt keeps its snapshot row");
        let embedding: i64 = rag_embeddings::table
            .filter(rag_embeddings::hash.eq(&old_hash))
            .count()
            .get_result(&mut conn)
            .unwrap();
        assert_eq!(embedding, 1, "p2.txt still uses the embedding");
    }
}
