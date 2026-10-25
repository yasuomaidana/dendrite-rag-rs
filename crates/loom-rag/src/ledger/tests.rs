use super::*;
use crate::chunk::{ChunkOptions, chunk_file};
use crate::lang::Lang;
use rstest::{fixture, rstest};
use std::path::PathBuf;

#[fixture]
fn ledger_env() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.redb");
    (dir, path)
}

#[rstest]
fn round_trips_files_chunks_embeddings_and_meta(ledger_env: (tempfile::TempDir, PathBuf)) {
    let (_dir, path) = ledger_env;
    let chunks = chunk_file(
        "a.go",
        Lang::Go,
        "package a\n\nfunc F() {}\n",
        ChunkOptions::default(),
    );
    {
        let ledger = Ledger::open(&path).unwrap();
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
    let ledger = Ledger::open(&path).unwrap();
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

#[rstest]
fn an_older_schema_is_cleared_but_keeps_embeddings(ledger_env: (tempfile::TempDir, PathBuf)) {
    let (_dir, path) = ledger_env;
    {
        let ledger = Ledger::open(&path).unwrap();
        let mut b = ledger.batch().unwrap();
        b.put_file(
            "a.go",
            &FileEntry {
                hash: "h".into(),
                size: 1,
                mtime_ns: 1,
                chunk_ids: Vec::new(),
                imports: Vec::new(),
            },
        )
        .unwrap();
        b.put_embedding("m:x", &[1.0]).unwrap();
        b.set_meta("merkle_root", "abc").unwrap();
        b.set_meta("schema", "1").unwrap();
        b.commit().unwrap();
    }
    let ledger = Ledger::open(&path).unwrap();
    assert!(ledger.files().unwrap().is_empty());
    assert_eq!(ledger.meta("merkle_root").unwrap(), None);
    assert_eq!(ledger.embedding_count().unwrap(), 1);
    assert_eq!(ledger.meta("schema").unwrap().as_deref(), Some(SCHEMA_VERSION));
}

#[rstest]
fn round_trips_llm_cache_and_concepts(ledger_env: (tempfile::TempDir, PathBuf)) {
    let (_dir, path) = ledger_env;
    {
        let ledger = Ledger::open(&path).unwrap();
        let mut b = ledger.batch().unwrap();
        b.put_llm_cache("gpt-4o", "hash123", "Community description")
            .unwrap();
        b.put_concepts("docs/spec.md", r#"[{"name":"RAG","description":"Retrieval"}]"#)
            .unwrap();
        b.commit().unwrap();
    }
    let ledger = Ledger::open(&path).unwrap();
    assert_eq!(
        ledger.llm_cache("gpt-4o", "hash123").unwrap().as_deref(),
        Some("Community description")
    );
    assert_eq!(
        ledger.concepts("docs/spec.md").unwrap().as_deref(),
        Some(r#"[{"name":"RAG","description":"Retrieval"}]"#)
    );

    // clear_index clears concepts but preserves llm_cache
    let mut b = ledger.batch().unwrap();
    b.clear_index().unwrap();
    b.commit().unwrap();
    assert_eq!(ledger.concepts("docs/spec.md").unwrap(), None);
    assert_eq!(
        ledger.llm_cache("gpt-4o", "hash123").unwrap().as_deref(),
        Some("Community description")
    );
}
