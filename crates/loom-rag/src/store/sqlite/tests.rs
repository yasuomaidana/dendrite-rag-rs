use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::chunk::{Chunk, ChunkOptions, chunk_file};
use crate::config::VectorIndexChoice;
use crate::engine::{Config, Engine};
use crate::graph::communities::Community;
use crate::lang::Lang;
use crate::ledger::{FileEntry, SCHEMA_VERSION};
use crate::store::{LedgerChoice, LedgerStore, LedgerWrite, StorageConfig, Store};
use crate::testutil::{HashEmbedder, project};

fn open(dir: &Path) -> SqliteLedger {
    SqliteLedger::open(&dir.join(INDEX_FILE)).unwrap()
}

/// Applies `f` to a batch and commits it.
fn put(ledger: &SqliteLedger, f: impl FnOnce(&mut dyn LedgerWrite)) {
    let mut b = ledger.batch().unwrap();
    f(&mut *b);
    b.commit().unwrap();
}

fn chunk(id: &str, path: &str, hash: &str) -> Chunk {
    let mut c = chunk_file(
        "a.go",
        Lang::Go,
        "package a\n\nfunc F() {}\n",
        ChunkOptions::default(),
    )
    .remove(0);
    c.id = id.into();
    c.path = path.into();
    c.content_hash = hash.into();
    c
}

fn entry(chunk_ids: &[&str]) -> FileEntry {
    FileEntry {
        hash: "h".into(),
        size: 3,
        mtime_ns: 1_700_000_000_000_000_000,
        chunk_ids: chunk_ids.iter().map(|s| s.to_string()).collect(),
        imports: vec!["fmt".into()],
    }
}

fn community(id: u32, files: &[&str]) -> Community {
    Community {
        id,
        label: format!("community {id}"),
        files: files.iter().map(|s| s.to_string()).collect(),
        top_symbols: Vec::new(),
        summary_chunk: None,
    }
}

fn raw(dir: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join(INDEX_FILE)).unwrap()
}

#[test]
fn round_trips_files_chunks_embeddings_and_meta() {
    let dir = tempfile::tempdir().unwrap();
    let chunks = chunk_file(
        "a.go",
        Lang::Go,
        "package a\n\nfunc F() {}\n",
        ChunkOptions::default(),
    );
    {
        let ledger = open(dir.path());
        let mut b = ledger.batch().unwrap();
        let entry = FileEntry {
            hash: "h".into(),
            size: 3,
            mtime_ns: 7,
            chunk_ids: vec![chunks[0].id.clone()],
            imports: vec!["fmt".into()],
        };
        b.put_file("a.go", &entry).unwrap();
        b.put_chunk(&chunks[0]).unwrap();
        b.put_embedding("m:x", &[1.0, -0.5]).unwrap();
        b.set_meta("merkle_root", "abc").unwrap();
        b.commit().unwrap();
    }
    let ledger = open(dir.path());
    assert_eq!(ledger.files().unwrap()["a.go"].mtime_ns, 7);
    assert_eq!(ledger.chunk(&chunks[0].id).unwrap().unwrap(), chunks[0]);
    assert_eq!(ledger.embedding("m:x").unwrap().unwrap(), vec![1.0, -0.5]);
    assert_eq!(ledger.meta("merkle_root").unwrap().as_deref(), Some("abc"));
    assert_eq!(ledger.meta("schema").unwrap().as_deref(), Some(SCHEMA_VERSION));

    let mut b = ledger.batch().unwrap();
    b.clear_index().unwrap();
    b.commit().unwrap();
    assert!(ledger.files().unwrap().is_empty());
    assert_eq!(ledger.chunk_count().unwrap(), 0);
    assert_eq!(ledger.embedding_count().unwrap(), 1);
}

#[test]
fn an_older_schema_is_cleared_but_keeps_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ledger = open(dir.path());
        put(&ledger, |b| {
            b.put_file("a.go", &entry(&["c1"])).unwrap();
            b.put_chunk(&chunk("c1", "a.go", "h1")).unwrap();
            b.put_concepts("a.go", "[]").unwrap();
            b.put_embedding("m\0h1", &[1.0]).unwrap();
            b.put_llm_cache("gpt", "p", "kept").unwrap();
            b.set_meta("merkle_root", "abc").unwrap();
            b.set_meta("schema", "1").unwrap();
        });
    }
    let ledger = open(dir.path());
    assert!(ledger.files().unwrap().is_empty());
    assert_eq!(ledger.chunk_count().unwrap(), 0);
    assert_eq!(ledger.concepts("a.go").unwrap(), None);
    assert_eq!(ledger.meta("merkle_root").unwrap(), None);
    assert_eq!(ledger.embedding_count().unwrap(), 1);
    assert_eq!(ledger.llm_cache("gpt", "p").unwrap().as_deref(), Some("kept"));
    assert_eq!(ledger.meta("schema").unwrap().as_deref(), Some(SCHEMA_VERSION));
}

#[test]
fn round_trips_llm_cache_and_concepts() {
    let dir = tempfile::tempdir().unwrap();
    let concepts = r#"[{"name":"RAG","description":"Retrieval"}]"#;
    {
        let ledger = open(dir.path());
        put(&ledger, |b| {
            b.put_llm_cache("gpt-4o", "hash123", "Community description")
                .unwrap();
            b.put_concepts("docs/spec.md", concepts).unwrap();
        });
    }
    let ledger = open(dir.path());
    assert_eq!(
        ledger.llm_cache("gpt-4o", "hash123").unwrap().as_deref(),
        Some("Community description")
    );
    assert_eq!(
        ledger.concepts("docs/spec.md").unwrap().as_deref(),
        Some(concepts)
    );

    // clear_index clears concepts but preserves llm_cache
    put(&ledger, |b| b.clear_index().unwrap());
    assert_eq!(ledger.concepts("docs/spec.md").unwrap(), None);
    assert_eq!(
        ledger.llm_cache("gpt-4o", "hash123").unwrap().as_deref(),
        Some("Community description")
    );

    put(&ledger, |b| {
        b.put_concepts("a.md", "[]").unwrap();
        b.remove_concepts("a.md").unwrap();
    });
    assert_eq!(ledger.concepts("a.md").unwrap(), None);
}

#[test]
fn files_come_back_in_path_order_and_can_be_replaced_or_removed() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    put(&ledger, |b| {
        for p in ["c.go", "a.go", "b/z.go", "b.go"] {
            b.put_file(p, &entry(&[])).unwrap();
        }
    });
    let paths: Vec<String> = ledger.files().unwrap().into_keys().collect();
    assert_eq!(paths, ["a.go", "b.go", "b/z.go", "c.go"]);

    put(&ledger, |b| {
        b.put_file("a.go", &entry(&["x"])).unwrap();
        b.remove_file("c.go").unwrap();
    });
    let files = ledger.files().unwrap();
    assert_eq!(files["a.go"], entry(&["x"]));
}

#[test]
fn chunks_keep_the_order_asked_for_and_skip_missing_ids() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    let c1 = chunk("c1", "a.go", "h1");
    let c2 = chunk("c2", "a.go", "h2");
    let c3 = chunk("c3", "b.go", "h3");
    put(&ledger, |b| {
        for c in [&c3, &c1, &c2] {
            b.put_chunk(c).unwrap();
        }
    });
    let ids = ["c3", "missing", "c1", "c2", "c3"].map(String::from);
    let got = ledger.chunks(&ids).unwrap();
    assert_eq!(got, [c3.clone(), c1.clone(), c2.clone(), c3.clone()]);
    assert_eq!(ledger.chunk("missing").unwrap(), None);

    put(&ledger, |b| b.remove_chunk("c2").unwrap());
    assert_eq!(ledger.chunk("c2").unwrap(), None);
    assert_eq!(ledger.chunk_count().unwrap(), 2);
}

#[test]
fn for_each_chunk_visits_every_chunk_in_id_order() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    put(&ledger, |b| {
        for id in ["c2", "c3", "c1"] {
            b.put_chunk(&chunk(id, "a.go", id)).unwrap();
        }
    });
    let mut seen = Vec::new();
    ledger.for_each_chunk(&mut |c| seen.push(c.id)).unwrap();
    assert_eq!(seen, ["c1", "c2", "c3"]);
}

#[test]
fn embeddings_come_back_in_the_order_of_the_keys() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    let (k1, k2) = ("model-a\0abc".to_string(), "model-b\0abc".to_string());
    put(&ledger, |b| {
        b.put_embedding(&k1, &[0.25, -1.5, 3.0]).unwrap();
        b.put_embedding(&k2, &[2.0]).unwrap();
    });
    let got = ledger
        .embeddings(&[k2.clone(), "model-a\0nope".to_string(), k1.clone()])
        .unwrap();
    assert_eq!(got, [Some(vec![2.0]), None, Some(vec![0.25, -1.5, 3.0])]);
    assert_eq!(ledger.embedding(&k1).unwrap().unwrap(), vec![0.25, -1.5, 3.0]);
    assert_eq!(ledger.embedding("model-a\0nope").unwrap(), None);
}

#[test]
fn communities_are_replaced_as_a_whole() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    let files = BTreeMap::from([
        ("a.go".to_string(), entry(&["c1", "c2"])),
        ("b.go".to_string(), entry(&["c3"])),
        ("c.go".to_string(), entry(&[])),
    ]);
    let (first, second) = (community(0, &["a.go", "b.go"]), community(1, &["c.go"]));
    put(&ledger, |b| {
        b.put_communities(&[second.clone(), first.clone()], &files)
            .unwrap()
    });
    let by_file = ledger.community_files().unwrap();
    assert_eq!(
        by_file,
        BTreeMap::from([
            ("a.go".to_string(), 0),
            ("b.go".to_string(), 0),
            ("c.go".to_string(), 1)
        ])
    );
    assert_eq!(ledger.community_of_chunk("c3").unwrap(), Some(0));
    assert_eq!(ledger.community_of_chunk("zz").unwrap(), None);
    assert_eq!(ledger.community(1).unwrap(), Some(second.clone()));
    assert_eq!(ledger.communities().unwrap(), [first, second.clone()]);

    put(&ledger, |b| {
        b.put_communities(std::slice::from_ref(&second), &files).unwrap()
    });
    assert_eq!(ledger.community(0).unwrap(), None);
    assert_eq!(ledger.community_of_chunk("c3").unwrap(), None);
    assert_eq!(ledger.community_files().unwrap().len(), 1);
    assert_eq!(ledger.communities().unwrap(), [second]);
}

#[test]
fn a_batch_is_invisible_until_it_commits() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    let mut b = ledger.batch().unwrap();
    b.put_file("a.go", &entry(&[])).unwrap();
    b.put_chunk(&chunk("c1", "a.go", "h1")).unwrap();
    assert!(ledger.files().unwrap().is_empty());
    drop(b);
    assert!(
        ledger.files().unwrap().is_empty(),
        "a dropped batch changes nothing"
    );

    let mut b = ledger.batch().unwrap();
    b.put_file("a.go", &entry(&[])).unwrap();
    b.put_chunk(&chunk("c1", "a.go", "h1")).unwrap();
    b.commit().unwrap();
    assert_eq!(ledger.files().unwrap().len(), 1);
    assert_eq!(ledger.chunk_count().unwrap(), 1);
}

#[test]
fn reads_run_while_another_thread_writes() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = open(dir.path());
    put(&ledger, |b| b.put_chunk(&chunk("c", "a.go", "h")).unwrap());
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..50 {
                    assert!(ledger.chunk_count().unwrap() >= 1);
                    assert!(ledger.chunk("c").unwrap().is_some());
                }
            });
        }
        s.spawn(|| {
            for i in 0..20 {
                put(&ledger, |b| {
                    b.put_chunk(&chunk(&format!("w{i}"), "a.go", "h")).unwrap()
                });
            }
        });
    });
    assert_eq!(ledger.chunk_count().unwrap(), 21);
}

#[test]
fn reopening_reads_everything_back() {
    let dir = tempfile::tempdir().unwrap();
    let files = BTreeMap::from([("a.go".to_string(), entry(&["c1"]))]);
    let c = community(0, &["a.go"]);
    {
        let ledger = open(dir.path());
        put(&ledger, |b| {
            b.put_file("a.go", &entry(&["c1"])).unwrap();
            b.put_chunk(&chunk("c1", "a.go", "h1")).unwrap();
            b.put_embedding("m\0h1", &[0.5, 0.25]).unwrap();
            b.put_llm_cache("gpt", "p", "text").unwrap();
            b.put_concepts("a.go", "[1]").unwrap();
            b.set_meta("embed_model", "m").unwrap();
            b.put_communities(std::slice::from_ref(&c), &files).unwrap();
        });
    }
    let ledger = open(dir.path());
    assert_eq!(ledger.files().unwrap(), files);
    assert_eq!(ledger.chunk("c1").unwrap().unwrap(), chunk("c1", "a.go", "h1"));
    assert_eq!(ledger.embedding("m\0h1").unwrap().unwrap(), vec![0.5, 0.25]);
    assert_eq!(ledger.llm_cache("gpt", "p").unwrap().as_deref(), Some("text"));
    assert_eq!(ledger.concepts("a.go").unwrap().as_deref(), Some("[1]"));
    assert_eq!(ledger.meta("embed_model").unwrap().as_deref(), Some("m"));
    assert_eq!(ledger.community_of_chunk("c1").unwrap(), Some(0));
    assert_eq!(ledger.communities().unwrap(), [c]);
}

#[test]
fn the_file_is_a_wal_database_at_the_current_ddl_version() {
    let dir = tempfile::tempdir().unwrap();
    drop(open(dir.path()));
    let conn = raw(dir.path());
    let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, schema::DDL_VERSION);
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
    let indexes: String = conn
        .query_row(
            "SELECT group_concat(name) FROM sqlite_master WHERE type = 'index' AND tbl_name = 'chunks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        indexes.contains("chunks_path") && indexes.contains("chunks_content_hash"),
        "{indexes}"
    );
}

#[test]
fn a_newer_ddl_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ledger = open(dir.path());
        put(&ledger, |b| b.set_meta("merkle_root", "abc").unwrap());
    }
    raw(dir.path()).execute_batch("PRAGMA user_version = 99").unwrap();

    let err = SqliteLedger::open(&dir.path().join(INDEX_FILE)).unwrap_err();
    assert!(
        matches!(&err, LedgerError::Backend(m) if m.contains("newer")),
        "{err}"
    );
    let conn = raw(dir.path());
    let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 99, "the file is left as it was");
    let root: String = conn
        .query_row("SELECT value FROM meta WHERE key = 'merkle_root'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(root, "abc");
}

#[test]
fn the_first_open_imports_the_caches_of_a_redb_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let redb = dir.path().join("ledger.redb");
    let key = "model-a\0abc";
    {
        let old = crate::ledger::Ledger::open(&redb).unwrap();
        let mut b = old.batch().unwrap();
        b.put_embedding(key, &[0.25, -1.5]).unwrap();
        b.put_embedding("m:x", &[7.0]).unwrap();
        b.put_llm_cache("gpt", "h1", "described").unwrap();
        b.put_file("a.go", &entry(&[])).unwrap();
        b.put_chunk(&chunk("c1", "a.go", "h1")).unwrap();
        b.commit().unwrap();
    }
    let before = std::fs::read(&redb).unwrap();

    let ledger = open(dir.path());
    assert_eq!(ledger.embedding(key).unwrap().unwrap(), vec![0.25, -1.5]);
    assert_eq!(ledger.embedding("m:x").unwrap().unwrap(), vec![7.0]);
    assert_eq!(ledger.embedding_count().unwrap(), 2);
    assert_eq!(
        ledger.llm_cache("gpt", "h1").unwrap().as_deref(),
        Some("described")
    );
    assert!(ledger.files().unwrap().is_empty(), "only the caches are imported");
    assert_eq!(ledger.chunk_count().unwrap(), 0);
    assert_eq!(std::fs::read(&redb).unwrap(), before, "ledger.redb is untouched");

    // Once imported, later opens leave the file alone, whatever redb holds now.
    drop(ledger);
    {
        let old = crate::ledger::Ledger::open(&redb).unwrap();
        let mut b = old.batch().unwrap();
        b.put_embedding("model-a\0later", &[1.0]).unwrap();
        b.commit().unwrap();
    }
    assert_eq!(open(dir.path()).embedding_count().unwrap(), 2);
}

#[test]
fn an_unreadable_redb_file_does_not_block_opening() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ledger.redb"), b"not a redb file").unwrap();
    let ledger = open(dir.path());
    assert_eq!(ledger.embedding_count().unwrap(), 0);
}

#[test]
fn the_store_opens_the_sqlite_ledger_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let store =
        Store::open_local_with(dir.path(), VectorIndexChoice::Auto, 64, LedgerChoice::Sqlite).unwrap();
    assert_eq!(store.ledger.chunk_count().unwrap(), 0);
    assert!(dir.path().join(INDEX_FILE).is_file());
    assert!(!dir.path().join("ledger.redb").exists());
}

fn sqlite_config() -> Config {
    Config {
        storage: StorageConfig {
            ledger: LedgerChoice::Sqlite,
            ..StorageConfig::default()
        },
        ..Config::default()
    }
}

#[test]
fn an_engine_on_the_sqlite_ledger_indexes_and_searches_like_the_redb_one() {
    let sqlite_dir = project();
    let redb_dir = project();
    let embedder = || Some(Box::new(HashEmbedder::new()) as Box<dyn crate::embed::Embedder>);
    let mut sqlite = Engine::open_with_embedder(sqlite_dir.path(), sqlite_config(), embedder()).unwrap();
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
    let mut sqlite = Engine::open_with_embedder(sqlite_dir.path(), sqlite_config(), embedder()).unwrap();
    let r = sqlite.index(false).unwrap();
    assert_eq!((r.files_changed, r.chunks_added, r.chunks_removed), (0, 0, 0));
    assert_eq!(sqlite.status().unwrap().chunks, st.chunks);
    assert_eq!(paths(&sqlite, "locate binary")[0], "pkg/sidecar/sidecar.go");
}

#[test]
fn switching_from_redb_to_sqlite_does_not_re_embed() {
    let dir = project();
    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();
    drop(e);
    assert!(calls.load(std::sync::atomic::Ordering::SeqCst) > 0);

    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let mut e = Engine::open_with_embedder(dir.path(), sqlite_config(), Some(Box::new(embedder))).unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.files_changed, 3, "the new ledger starts without files");
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "embeddings came from the cache"
    );
}
