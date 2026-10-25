use super::*;
use crate::store::VECTORS_FILE;
use crate::testutil::{HashEmbedder, project};
use crate::vector::FlatIndex;
use std::fs;

mod llm;
mod vectors;

fn open(root: &Path) -> Engine {
    Engine::open_with_embedder(root, Config::default(), None).unwrap()
}

#[test]
fn embedding_budget_defers_work_to_later_runs() {
    let dir = project();
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .unwrap();
    let r = e.index_with(false, Some(std::time::Duration::ZERO)).unwrap();
    assert_eq!(r.embedded, 0);
    assert_eq!(r.embed_pending as u64, e.status().unwrap().chunks);
    let st = e.status().unwrap();
    assert_eq!(st.embed_coverage, Some(0.0));
    assert_eq!(st.warnings.len(), 1, "low coverage is reported");
    assert_eq!(
        e.search_lexical("LocateBinary", 1).unwrap().len(),
        1,
        "lexical index is complete"
    );

    let r = e.index(false).unwrap();
    assert_eq!((r.files_changed, r.embed_pending), (0, 0));
    assert_eq!(r.embedded as u64, e.status().unwrap().chunks);
    let st = e.status().unwrap();
    assert_eq!((st.embed_coverage, st.warnings.len()), (Some(1.0), 0));
}

#[test]
fn embedding_failures_are_reported_and_retried() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let failing = HashEmbedder::new();
    failing.fail.store(true, Ordering::SeqCst);
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(failing))).unwrap();
    let r = e.index(false).unwrap();
    assert!(r.embed_error.as_deref().unwrap().contains("gateway down"));
    assert_eq!(
        r.embed_pending as u64,
        e.status().unwrap().chunks,
        "a failed embed leaves every chunk pending"
    );
    assert_eq!(e.search_lexical("LocateBinary", 1).unwrap().len(), 1);
    drop(e);

    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .unwrap();
    let r = e.index(false).unwrap();
    assert!(r.embed_error.is_none());
    assert_eq!(r.files_changed, 0);
    assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);
}

#[test]
fn indexes_then_skips_unchanged_files() {
    let dir = project();
    let mut e = open(dir.path());
    let r = e.index(false).unwrap();
    assert_eq!((r.files_scanned, r.files_changed, r.files_removed), (3, 3, 0));
    assert!(r.chunks_added >= 3);
    let r = e.index(false).unwrap();
    assert_eq!((r.files_changed, r.chunks_added, r.chunks_removed), (0, 0, 0));
    assert_eq!(
        e.search_lexical("locate binary", 3).unwrap()[0].path,
        "pkg/sidecar/sidecar.go"
    );
}

#[test]
fn edits_deletes_and_renames_are_picked_up() {
    let dir = project();
    let root = dir.path();
    let mut e = open(root);
    e.index(false).unwrap();

    fs::write(
        root.join("pkg/rag/indexer.go"),
        "package rag\n\nfunc RetrieveChunks() {}\n",
    )
    .unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.files_changed, 1);
    assert!(e.search_lexical("IndexProject", 3).unwrap().is_empty());
    assert_eq!(
        e.search_lexical("retrieve chunks", 3).unwrap()[0].path,
        "pkg/rag/indexer.go"
    );

    fs::rename(root.join("pkg/sidecar"), root.join("pkg/helper")).unwrap();
    let r = e.index(false).unwrap();
    assert_eq!((r.files_changed, r.files_removed), (1, 1));
    let hits = e.search_lexical("LocateBinary", 3).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "pkg/helper/sidecar.go");

    fs::remove_file(root.join("README.md")).unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.files_removed, 1);
    assert_eq!(e.status().unwrap().files, 2);
}

#[test]
fn touching_without_changes_reindexes_nothing() {
    let dir = project();
    let root = dir.path();
    let mut e = open(root);
    e.index(false).unwrap();
    let p = root.join("README.md");
    let content = fs::read(&p).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&p, content).unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.files_changed, 0);
}

#[test]
fn full_rebuild_status_and_purge() {
    let dir = project();
    let mut e = open(dir.path());
    e.index(false).unwrap();
    let before = e.status().unwrap();
    let r = e.index(true).unwrap();
    assert_eq!(r.files_changed, 3);
    let after = e.status().unwrap();
    assert_eq!(before.chunks, after.chunks);
    assert_eq!(before.merkle_root, after.merkle_root);
    assert!(after.last_indexed.is_some());
    let index_dir = PathBuf::from(&after.index_dir);
    assert!(index_dir.join(".gitignore").exists());
    e.purge().unwrap();
    assert!(!index_dir.exists());
}

struct SharedEmbedder(Arc<HashEmbedder>);

impl Embedder for SharedEmbedder {
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

#[test]
fn index_stores_communities_per_file_and_chunk() {
    let dir = project();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
    let r = e.index(false).unwrap();
    assert!(r.communities > 0);
    let files = e.ledger.community_files().unwrap();
    assert_eq!(files.len(), e.ledger.files().unwrap().len());
    let (path, id) = files.iter().next().unwrap();
    let entry = &e.ledger.files().unwrap()[path];
    if let Some(chunk) = entry.chunk_ids.first() {
        assert_eq!(e.ledger.community_of_chunk(chunk).unwrap(), Some(*id));
    }
    assert!(e.ledger.community(*id).unwrap().unwrap().files.contains(path));
    assert_eq!(e.status().unwrap().communities, r.communities);
    // A no-op run keeps them without recomputing.
    let again = e.index(false).unwrap();
    assert_eq!(again.communities, r.communities);
    assert_eq!(again.graph_ms, 0);
}

#[test]
fn a_cleared_ledger_rebuilds_the_lexical_index() {
    let dir = project();
    let mut e = open(dir.path());
    e.index(false).unwrap();
    let docs = e.lexical.len();
    drop(e);
    {
        // An older schema version: the next open clears files and chunks.
        let ledger =
            crate::ledger::Ledger::open(&dir.path().join(DEFAULT_INDEX_DIR).join("ledger.redb")).unwrap();
        let mut b = ledger.batch().unwrap();
        b.set_meta("schema", "1").unwrap();
        b.commit().unwrap();
    }
    let mut e = open(dir.path());
    e.index(false).unwrap();
    assert_eq!(e.lexical.len(), docs, "lexical documents must not be duplicated");
}

/// A project of `n` Go files, each defining one uniquely named function.
fn many_files(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        let d = dir.path().join(format!("pkg/p{}", i / 100));
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join(format!("f{i}.go")),
            format!("package p\n\nfunc Unique{i}Marker() {{}}\n"),
        )
        .unwrap();
    }
    dir
}

#[test]
fn cold_index_commits_in_groups_and_stays_consistent() {
    let n = 2 * index::CHUNK_GROUP_FILES + 7;
    let dir = many_files(n);
    let mut e = open(dir.path());
    let r = e.index(false).unwrap();
    assert_eq!((r.files_scanned, r.files_changed), (n, n));
    assert_eq!(e.lexical.len(), e.ledger.chunk_count().unwrap());
    assert_eq!(
        e.search_lexical(&format!("Unique{}Marker", n - 1), 5)
            .unwrap()
            .len(),
        1
    );
    let r = e.index(false).unwrap();
    assert_eq!((r.files_changed, r.chunks_added), (0, 0));
}

#[test]
fn a_group_committed_only_to_the_lexical_index_is_not_duplicated() {
    let dir = many_files(3);
    let mut e = open(dir.path());
    e.index(false).unwrap();
    // A crash after the lexical commit and before the ledger's: the lexical index holds
    // the file's chunks, the ledger does not know the file.
    let path = "pkg/p0/f1.go";
    let entry = e.ledger.files().unwrap()[path].clone();
    let mut b = e.ledger.batch().unwrap();
    for id in &entry.chunk_ids {
        b.remove_chunk(id).unwrap();
    }
    b.remove_file(path).unwrap();
    b.commit().unwrap();

    let r = e.index(false).unwrap();
    assert_eq!(r.files_changed, 1);
    assert_eq!(e.search_lexical("Unique1Marker", 5).unwrap().len(), 1);
    assert_eq!(e.lexical.len(), e.ledger.chunk_count().unwrap());
}

#[test]
fn chunk_contexts_are_indexed_but_never_returned() {
    let dir = project();
    let mut probe = open(dir.path());
    probe.index(false).unwrap();
    let mut ids = Vec::new();
    probe
        .ledger
        .for_each_chunk(&mut |c| {
            if c.path == "pkg/rag/indexer.go" {
                ids.push(c.id)
            }
        })
        .unwrap();
    drop(probe);

    let contexts = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                "walks the repository with a zebra crawler".to_string(),
            )
        })
        .collect();
    let cfg = Config {
        index_dir: Some(dir.path().join(".loom-ctx")),
        chunk_contexts: Some(Arc::new(contexts)),
        ..Config::default()
    };
    let mut e = Engine::open_with_embedder(dir.path(), cfg, None).unwrap();
    e.index(false).unwrap();
    let hits = e.search_lexical("zebra crawler", 3).unwrap();
    assert_eq!(hits[0].path, "pkg/rag/indexer.go", "the context is searchable");
    assert!(!hits[0].content.contains("zebra"), "and never returned");
    assert!(
        open(dir.path())
            .search_lexical("zebra crawler", 3)
            .unwrap()
            .is_empty()
    );
}
