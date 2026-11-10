use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::engine::Engine;
use crate::store::StorageMode;
use crate::testutil::{HashEmbedder, engine_with, pg_storage, project, unique};

fn open(
    root: &std::path::Path,
    index: &str,
    mode: StorageMode,
    url: &str,
    repo: &str,
    e: &Arc<HashEmbedder>,
) -> Engine {
    engine_with(root, index, pg_storage(mode, url, repo), Some(e))
}

#[test]
fn journals_while_down_and_replays_once_up() {
    let Some(pg) = crate::testutil::test_pg() else {
        return;
    };
    let url = pg.url.clone();
    let dir = project();
    let repo = unique("edge");
    let embedder = Arc::new(HashEmbedder::new());
    // Nothing listens on port 1: every mirror goes to the journal.
    let mut down = open(
        dir.path(),
        ".loom-edge",
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
        ".loom-edge",
        StorageMode::Edge,
        &url,
        &repo,
        &embedder,
    );
    assert_eq!(up.status().unwrap().sync_pending, Some(0));
    assert_eq!(up.sync().unwrap(), 0, "a second replay has nothing to send");
    let remote = open(
        dir.path(),
        ".loom-remote",
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
    let Some(pg) = crate::testutil::test_pg() else {
        return;
    };
    let url = pg.url.clone();
    let dir = project();
    // Someone else's machine indexed the same content into the shared tier.
    let first = Arc::new(HashEmbedder::new());
    let mut theirs = open(
        dir.path(),
        ".loom-a",
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
        ".loom-b",
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

#[test]
fn a_torn_journal_tail_does_not_swallow_the_next_batch() {
    let dir = project();
    let journal = dir.path().join(super::JOURNAL_FILE);
    std::fs::write(&journal, "{\"cut").unwrap();
    let edge = super::Edge::new("postgresql://rag:rag@127.0.0.1:1/x", "torn", journal.clone());
    edge.append("{}").unwrap();
    let text = std::fs::read_to_string(&journal).unwrap();
    assert_eq!(text, "{\"cut\n{}\n");
    assert_eq!(edge.pending(), 2);
}

#[test]
fn an_unparsable_journal_line_is_kept_with_everything_after_it() {
    let Some(pg) = crate::testutil::test_pg() else {
        return;
    };
    let dir = project();
    let journal = dir.path().join(super::JOURNAL_FILE);
    let content = "{\"cut\n{\"later\":1}\n";
    std::fs::write(&journal, content).unwrap();
    let edge = super::Edge::new(&pg.url, &unique("replay"), journal.clone());
    // The tier is up, so the replay reaches the bad line: it stops there and keeps it.
    assert_eq!(edge.replay().unwrap(), 2, "both lines stay pending");
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), content);
    assert_eq!(edge.pending(), 2);
}
