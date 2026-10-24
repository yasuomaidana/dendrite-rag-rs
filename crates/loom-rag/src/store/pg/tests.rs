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

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

fn open(root: &std::path::Path, storage: StorageConfig, embedder: &Arc<HashEmbedder>) -> Engine {
    let cfg = Config {
        index_dir: Some(root.join(".loom-test")),
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
        ..StorageConfig::default()
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
    let Some(pg) = crate::testutil::test_pg() else {
        return;
    };
    let url = pg.url.clone();
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
    let Some(pg) = crate::testutil::test_pg() else {
        return;
    };
    let url = pg.url.clone();
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

#[test]
fn configured_bm25_ranks_alike_in_sql_and_tantivy() {
    let Some(pg) = crate::testutil::test_pg() else {
        return;
    };
    let params = crate::bm25::Bm25Params {
        k1: 0.9,
        b: 0.4,
        q: 0.5,
    };
    let dir = project();
    let embedder = Arc::new(HashEmbedder::new());
    let mut local = open(dir.path(), StorageConfig::default(), &embedder);
    local.set_bm25(params);
    local.index(false).unwrap();
    let mut remote_e = open(dir.path(), remote(&pg.url, &unique("bm25")), &embedder);
    remote_e.set_bm25(params);
    remote_e.index(false).unwrap();
    for q in [
        "LocateBinary",
        "IndexProject",
        "sidecar binary",
        "project used in tests",
    ] {
        assert_eq!(
            ids(&remote_e, q, Mode::Lexical).first(),
            ids(&local, q, Mode::Lexical).first(),
            "{q}"
        );
    }
}
