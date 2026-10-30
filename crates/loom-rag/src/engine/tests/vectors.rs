//! Vector index and query-embedding cache tests.

use super::*;

#[test]
fn dense_search_uses_cached_embeddings() {
    let dir = project();
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.embedded, r.chunks_added);
    assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);
    let hits = e.search_dense("LocateBinary sidecar", 1).unwrap();
    assert_eq!(hits[0].path, "pkg/sidecar/sidecar.go");

    // A full rebuild re-chunks everything but embeds nothing new.
    let r = e.index(true).unwrap();
    assert_eq!(r.embedded, 0);
    assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);

    // Reopening loads the persisted vectors.
    drop(e);
    let e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
        .unwrap();
    assert!(e.has_vectors());
    assert_eq!(
        e.search_dense("IndexProject", 1).unwrap()[0].path,
        "pkg/rag/indexer.go"
    );
}

#[test]
fn truncated_or_corrupt_vectors_heals_from_cache() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let embedder = Arc::new(HashEmbedder::new());
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder.clone()))).unwrap();
    let r = e.index(false).unwrap();
    assert_eq!(r.embedded, r.chunks_added);
    assert!(e.status().unwrap().vectors > 0);
    let hits_before = e.search_dense("LocateBinary sidecar", 1).unwrap();
    assert_eq!(hits_before[0].path, "pkg/sidecar/sidecar.go");
    let initial_calls = embedder.calls.load(Ordering::SeqCst);
    assert!(initial_calls > 0);
    drop(e);

    // Corrupt vectors.bin by truncating it
    let vectors_path = dir.path().join(DEFAULT_INDEX_DIR).join(VECTORS_FILE);
    assert!(vectors_path.exists());
    fs::write(&vectors_path, b"DRV3-truncated").unwrap();

    // Engine::open succeeds on corrupt vectors.bin
    let embedder = Arc::new(HashEmbedder::new());
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder.clone()))).unwrap();
    assert_eq!(e.status().unwrap().vectors, 0);

    // After index, dense search works with zero embed calls
    let r = e.index(false).unwrap();
    assert_eq!(r.embedded, 0);
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);

    let hits_after = e.search_dense("LocateBinary sidecar", 1).unwrap();
    assert_eq!(hits_after[0].path, "pkg/sidecar/sidecar.go");

    // Now corrupt the BLAKE3 checksum footer
    drop(e);
    let mut bytes = fs::read(&vectors_path).unwrap();
    let len = bytes.len();
    bytes[len - 1] ^= 0xff;
    fs::write(&vectors_path, &bytes).unwrap();

    let embedder = Arc::new(HashEmbedder::new());
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder.clone()))).unwrap();
    assert_eq!(e.status().unwrap().vectors, 0);

    let r = e.index(false).unwrap();
    assert_eq!(r.embedded, 0);
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.status().unwrap().vectors as u64, e.status().unwrap().chunks);

    let hits_after2 = e.search_dense("LocateBinary sidecar", 1).unwrap();
    assert_eq!(hits_after2[0].path, "pkg/sidecar/sidecar.go");
}

#[test]
fn sync_vectors_detects_mismatched_ids_with_equal_count() {
    let dir = project();
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .unwrap();
    e.index(false).unwrap();
    let total = e.status().unwrap().chunks as usize;
    assert!(total > 0);
    let vectors_path = dir.path().join(DEFAULT_INDEX_DIR).join(VECTORS_FILE);

    // Replace one valid ID with an alien ID, keeping the same count
    let mut idx = FlatIndex::load(&vectors_path).unwrap();
    let first_id = idx.ids().next().unwrap().to_string();
    let tag = idx.tag(&first_id).unwrap().to_string();
    idx.remove(&first_id);
    idx.upsert("alien-chunk-id", &tag, &vec![0.1f32; 64]);
    assert_eq!(idx.len(), total);
    idx.save(&vectors_path).unwrap();

    // Reopen with this modified vectors.bin
    drop(e);
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .unwrap();
    // Count matches, but IDs do not match ledger chunk IDs
    assert_eq!(e.status().unwrap().vectors, total);
    assert!(!e.vectors.contains(&first_id));
    assert!(e.vectors.contains("alien-chunk-id"));

    // index(false) must detect the mismatch, remove alien id, and refill the missing id from cache
    let r = e.index(false).unwrap();
    assert_eq!(r.embedded, 0);
    assert_eq!(e.status().unwrap().vectors, total);
    assert!(e.vectors.contains(&first_id));
    assert!(!e.vectors.contains("alien-chunk-id"));
}

#[test]
fn query_cache_skips_repeated_embeddings() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();

    let initial_calls = calls.load(Ordering::SeqCst);

    // First query embeds and populates the cache.
    let hits1 = e.dense_ranking("LocateBinary sidecar", 1).unwrap();
    assert!(!hits1.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 1);
    assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 1);

    // Second identical query hits the LRU cache without calling the embedder.
    let hits2 = e.dense_ranking("LocateBinary sidecar", 1).unwrap();
    assert_eq!(hits1, hits2);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        initial_calls + 1,
        "repeated query must not call the embedder"
    );

    // A different query calls the embedder again.
    let hits3 = e.dense_ranking("IndexProject", 1).unwrap();
    assert!(!hits3.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);
    assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 2);
}

#[test]
fn query_cache_is_keyed_by_model() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .unwrap();
    e.index(false).unwrap();
    e.dense_ranking("LocateBinary", 1).unwrap();
    let other = HashEmbedder::with_model("hash-64-v2");
    let calls = other.calls.clone();
    e.embedder = Some(Box::new(other));
    e.dense_ranking("LocateBinary", 1).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "another model must not reuse the vector"
    );
}

#[test]
fn query_cache_disabled_when_capacity_is_zero() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let cfg = Config {
        query_cache: 0,
        ..Config::default()
    };
    let mut e = Engine::open_with_embedder(dir.path(), cfg, Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();

    let initial_calls = calls.load(Ordering::SeqCst);

    e.dense_ranking("LocateBinary", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 1);
    assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 0);

    // Second identical query still calls the embedder because cache is disabled.
    e.dense_ranking("LocateBinary", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);
    assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 0);
}

#[test]
fn query_cache_evicts_lru_when_capacity_reached() {
    use std::sync::atomic::Ordering;
    let dir = project();
    let embedder = HashEmbedder::new();
    let calls = embedder.calls.clone();
    let cfg = Config {
        query_cache: 2,
        ..Config::default()
    };
    let mut e = Engine::open_with_embedder(dir.path(), cfg, Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();

    let initial_calls = calls.load(Ordering::SeqCst);

    e.dense_ranking("q1", 1).unwrap();
    e.dense_ranking("q2", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);
    assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 2);

    // Access q1 so q2 becomes the least recently used.
    e.dense_ranking("q1", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 2);

    // Insert q3, evicting q2.
    e.dense_ranking("q3", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 3);
    assert_eq!(e.query_cache.as_ref().map_or(0, |c| c.lock().unwrap().len()), 2);

    // q1 is still in cache.
    e.dense_ranking("q1", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 3);

    // q2 was evicted, so it calls the embedder again.
    e.dense_ranking("q2", 1).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), initial_calls + 4);
}

/// Records the texts it embeds and takes `delay` per call.
struct SlowEmbedder {
    inner: HashEmbedder,
    delay: std::time::Duration,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Embedder for SlowEmbedder {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn embed_documents(&self, texts: &[String]) -> crate::embed::Result<Vec<Vec<f32>>> {
        std::thread::sleep(self.delay);
        self.seen.lock().unwrap().extend(texts.iter().cloned());
        self.inner.embed_documents(texts)
    }

    fn embed_query(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        self.inner.embed_query(text)
    }
}

#[test]
fn a_budgeted_run_embeds_the_focus_question_first() {
    let dir = project();
    for i in 0..30 {
        fs::write(
            dir.path().join(format!("pkg/rag/filler{i}.go")),
            format!("package rag\n\nfunc Filler{i}() int {{ return {i} }}\n"),
        )
        .unwrap();
    }
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let embedder = SlowEmbedder {
        inner: HashEmbedder::new(),
        delay: std::time::Duration::from_millis(1500),
        seen: seen.clone(),
    };
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
    // The first run indexes lexically; the second has nothing else to do, so its budget
    // leaves time for exactly one embedding group (a delay well above the budget keeps a slow runner,
    // which spends part of the budget walking the tree, from running none).
    e.index_with(false, Some(std::time::Duration::ZERO)).unwrap();
    let r = e
        .index_focused(
            false,
            Some(std::time::Duration::from_millis(800)),
            Some("LocateBinary sidecar"),
        )
        .unwrap();
    assert!(r.embed_pending > 0, "the budget stops after one group: {r:?}");
    let seen = seen.lock().unwrap();
    // 1 chunk of 33 defines LocateBinary: first by chance 3% of the time.
    assert!(
        seen[0].contains("LocateBinary"),
        "the focus chunk is embedded first: {seen:?}"
    );
}

struct DimensionMockEmbedder {
    model: String,
    dim: usize,
}

impl Embedder for DimensionMockEmbedder {
    fn model_id(&self) -> &str {
        &self.model
    }

    fn embed_documents(&self, texts: &[String]) -> crate::embed::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0; self.dim]).collect())
    }

    fn embed_query(&self, _text: &str) -> crate::embed::Result<Vec<f32>> {
        Ok(vec![1.0; self.dim])
    }
}

#[test]
fn dimensions_cache_keys_do_not_collide() {
    let hash = "abc123def456";
    let key1024 = cache_key("text-embedding-3-large@1024", hash);
    let key3072 = cache_key("text-embedding-3-large@3072", hash);
    assert_ne!(key1024, key3072);

    let dir = project();
    let emb1024 = DimensionMockEmbedder {
        model: "text-embedding-3-large@1024".into(),
        dim: 1024,
    };
    let mut e1 = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(emb1024))).unwrap();
    let r1 = e1.index(false).unwrap();
    assert_eq!(r1.embedded, r1.chunks_added);
    drop(e1);

    let emb3072 = DimensionMockEmbedder {
        model: "text-embedding-3-large@3072".into(),
        dim: 3072,
    };
    let mut e2 = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(emb3072))).unwrap();
    let r2 = e2.index(false).unwrap();
    assert_eq!(
        r2.embedded, r1.embedded,
        "3072-d vectors must be re-embedded, not colliding with 1024-d cache"
    );
}

#[test]
fn budget_tokens_caps_embedding_run() {
    let dir = project();
    for i in 0..20 {
        fs::write(
            dir.path().join(format!("pkg/rag/token_filler{i}.go")),
            format!("package rag\n\nfunc TokenFiller{i}() string {{ return \"long filler code to consume tokens\" }}\n"),
        )
        .unwrap();
    }
    let mut cfg = Config::default();
    // Tiny token budget to embed only 1 group
    cfg.embed.budget_tokens = Some(50);
    let mut e = Engine::open_with_embedder(dir.path(), cfg, Some(Box::new(HashEmbedder::new()))).unwrap();
    let r = e.index(false).unwrap();
    assert!(
        r.embed_pending > 0,
        "token budget must cap the embedding run: {r:?}"
    );
    assert!(r.embed_tokens > 0, "must report tokens used: {r:?}");
    assert!(
        r.budget_tokens_remaining.is_some(),
        "must report remaining budget: {r:?}"
    );
}
