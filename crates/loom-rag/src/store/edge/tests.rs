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
    std::env::var("LOOM_TEST_PG_URL").ok().filter(|u| !u.is_empty())
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
    let Some(url) = url() else { return };
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
