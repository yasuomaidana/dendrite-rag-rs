//! The ledger choice end to end: the store and an engine on the SQLite ledger.

use super::*;
use rstest::{fixture, rstest};

#[test]
fn the_store_opens_the_sqlite_ledger_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        Store::open_local_with(dir.path(), VectorIndexChoice::Auto, 64, LedgerChoice::Sqlite).unwrap();
    assert_eq!(store.ledger.chunk_count().unwrap(), 0);
    assert!(dir.path().join(INDEX_FILE).is_file());
    assert!(!dir.path().join("ledger.redb").exists());
}

#[fixture]
fn sqlite_config() -> Config {
    Config {
        storage: StorageConfig {
            ledger: LedgerChoice::Sqlite,
            ..StorageConfig::default()
        },
        ..Config::default()
    }
}

#[rstest]
fn an_engine_on_the_sqlite_ledger_indexes_and_searches_like_the_redb_one(sqlite_config: Config) {
    let sqlite_dir = project();
    let redb_dir = project();
    let embedder = || Some(Box::new(HashEmbedder::new()) as Box<dyn crate::embed::Embedder>);
    let mut sqlite = Engine::open_with_embedder(sqlite_dir.path(), sqlite_config, embedder()).unwrap();
    let mut redb = Engine::open_with_embedder(redb_dir.path(), Config::default(), embedder()).unwrap();

    let a = sqlite.index(false).unwrap();
    let b = redb.index(false).unwrap();
    assert_eq!((a.files_scanned, a.files_changed, a.files_removed), (3, 3, 0));
    assert_eq!(
        (a.files_changed, a.chunks_added, a.embedded),
        (b.files_changed, b.chunks_added, b.embedded)
    );
    assert!(sqlite_dir.path().join(".loom").join(INDEX_FILE).is_file());
    assert!(!sqlite_dir.path().join(".loom/ledger.redb").exists());

    let paths = |e: &Engine, q: &str| -> Vec<String> {
        e.search_lexical(q, 3)
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect()
    };
    for q in ["locate binary", "IndexProject", "retrieval"] {
        assert_eq!(paths(&sqlite, q), paths(&redb, q), "{q}");
    }
    assert_eq!(paths(&sqlite, "locate binary")[0], "pkg/sidecar/sidecar.go");
    assert!(!sqlite.search_dense("locate binary", 3).unwrap().is_empty());
    let (st, rt) = (sqlite.status().unwrap(), redb.status().unwrap());
    assert_eq!(
        (st.files, st.chunks, st.vectors),
        (rt.files, rt.chunks, rt.vectors)
    );

    drop(sqlite);
    let mut sqlite = Engine::open_with_embedder(sqlite_dir.path(), sqlite_config, embedder()).unwrap();
    let r = sqlite.index(false).unwrap();
    assert_eq!((r.files_changed, r.chunks_added, r.chunks_removed), (0, 0, 0));
    assert_eq!(sqlite.status().unwrap().chunks, st.chunks);
    assert_eq!(paths(&sqlite, "locate binary")[0], "pkg/sidecar/sidecar.go");
}

#[rstest]
fn switching_from_redb_to_sqlite_does_not_re_embed(sqlite_config: Config) {
    let dir = project();
    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();
    drop(e);
    assert!(calls.load(std::sync::atomic::Ordering::SeqCst) > 0);

    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let mut e = Engine::open_with_embedder(dir.path(), sqlite_config, Some(Box::new(embedder))).unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.files_changed, 3, "the new ledger starts without files");
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "embeddings came from the cache"
    );
}
