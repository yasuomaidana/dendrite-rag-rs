use super::pack::merge_adjacent;
use super::*;
use crate::corpus::CorpusFilter;
use crate::engine::Config;
use crate::testutil::{HashEmbedder, project};
use std::fs;

fn engine(root: &std::path::Path) -> Engine {
    let mut e =
        Engine::open_with_embedder(root, Config::default(), Some(Box::new(HashEmbedder::new()))).unwrap();
    e.index(false).unwrap();
    e
}

fn calls_project() -> tempfile::TempDir {
    let dir = project();
    fs::write(
        dir.path().join("pkg/sidecar/client.go"),
        "package sidecar\n\n// Start launches a helper process.\nfunc Start(name string) error {\n\tpath := LocateBinary(name)\n\treturn run(path)\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn hybrid_fuses_lexical_dense_and_symbols() {
    let dir = calls_project();
    let e = engine(dir.path());
    let hits = e.search("LocateBinary", &SearchOpts::default()).unwrap();
    assert_eq!(
        hits[0].path, "pkg/sidecar/sidecar.go",
        "the definition ranks first"
    );
    assert!(
        hits.iter().any(|h| h.path == "pkg/sidecar/client.go"),
        "the caller is found too"
    );

    for mode in [Mode::Lexical, Mode::Dense] {
        let hits = e
            .search(
                "index project",
                &SearchOpts {
                    mode,
                    ..SearchOpts::default()
                },
            )
            .unwrap();
        assert_eq!(hits[0].path, "pkg/rag/indexer.go", "{mode:?}");
    }
}

#[test]
fn path_globs_filter_results() {
    let dir = calls_project();
    let e = engine(dir.path());
    let opts = SearchOpts {
        paths: vec!["**/*.md".into()],
        ..SearchOpts::default()
    };
    let hits = e.search("project", &opts).unwrap();
    assert!(!hits.is_empty() && hits.iter().all(|h| h.path.ends_with(".md")));
}

#[test]
fn context_expands_through_the_symbol_graph_within_budget() {
    let dir = calls_project();
    let e = engine(dir.path());
    let pack = e.context("Start helper process", 2000).unwrap();
    let paths: Vec<&str> = pack.spans.iter().map(|s| s.path.as_str()).collect();
    assert!(paths.contains(&"pkg/sidecar/client.go"));
    assert!(
        paths.contains(&"pkg/sidecar/sidecar.go"),
        "the callee is pulled in: {paths:?}"
    );
    assert!(pack.tokens <= 2000);

    // Every chunk of this small project is a retrieved hit, so none is a pure neighbour.
    assert!(pack.spans.iter().all(|s| !s.outline));
    let full = e
        .context_with("Start helper process", 2000, CorpusFilter::All, Detail::Full)
        .unwrap();
    assert!(full.spans.iter().all(|s| !s.outline));
    let map = e
        .context_with("Start helper process", 2000, CorpusFilter::All, Detail::Map)
        .unwrap();
    assert!(map.spans.iter().all(|s| s.outline) && map.tokens < full.tokens);
    let callee = map
        .spans
        .iter()
        .find(|s| s.path == "pkg/sidecar/sidecar.go")
        .unwrap();
    assert_eq!(
        callee.content,
        "// LocateBinary finds a sidecar executable.\nfunc LocateBinary(name string) string { return name }"
    );
    let bodies = e.chunks(&[callee.chunk_id.clone(), "unknown".into()]).unwrap();
    assert_eq!(bodies.len(), 1);
    assert!(!bodies[0].outline && bodies[0].content.contains("return name"));

    let tiny = e.context("Start helper process", 40).unwrap();
    assert!(tiny.tokens <= 40);
    assert!(tiny.spans.len() < pack.spans.len());
}

#[test]
fn adjacent_spans_merge() {
    let hit = |start, end, content: &str| Hit {
        chunk_id: format!("{start}"),
        path: "a.go".into(),
        start_line: start,
        end_line: end,
        scope: vec![],
        score: start as f32,
        content: content.into(),
        duplicates: vec![],
        outline: false,
    };
    let merged = merge_adjacent(vec![hit(5, 9, "b"), hit(1, 4, "a"), hit(20, 22, "c")]);
    assert_eq!(merged.len(), 2);
    let first = merged.iter().find(|h| h.start_line == 1).unwrap();
    assert_eq!((first.end_line, first.content.as_str()), (9, "a\nb"));
}

#[test]
fn hybrid_degrades_to_lexical_when_query_embedding_fails() {
    use std::sync::atomic::Ordering;
    let dir = calls_project();
    let embedder = HashEmbedder::new();
    embedder.fail_queries.store(true, Ordering::SeqCst);
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
    e.index(false).unwrap();
    assert!(e.has_vectors());
    let hits = e.search("LocateBinary", &SearchOpts::default()).unwrap();
    assert_eq!(hits[0].path, "pkg/sidecar/sidecar.go");
    let dense = SearchOpts {
        mode: Mode::Dense,
        ..SearchOpts::default()
    };
    assert!(matches!(e.search("LocateBinary", &dense), Err(Error::Embed(_))));
}

#[test]
fn community_summaries_rank_and_expand_to_code() {
    use crate::graph::summary::is_summary;
    let dir = calls_project();
    let mut e = engine(dir.path());
    let summaries = |e: &Engine| {
        let mut n = 0;
        e.ledger
            .for_each_chunk(&mut |c| n += is_summary(&c.id) as usize)
            .unwrap();
        n
    };
    let n = summaries(&e);
    assert!(n > 0, "a community of two or more files has a summary chunk");
    // The summary lists its files: a query naming them finds the community's code.
    let hits = e
        .search("community files sidecar.go client.go", &SearchOpts::default())
        .unwrap();
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| !is_summary(&h.chunk_id)),
        "summaries never surface"
    );
    // Re-indexing after a change replaces summaries instead of adding more.
    fs::write(
        dir.path().join("pkg/sidecar/client.go"),
        "package sidecar\n\nfunc Start(name string) error {\n\treturn run(LocateBinary(name))\n}\n",
    )
    .unwrap();
    e.index(false).unwrap();
    assert_eq!(summaries(&e), n);
    assert_eq!(e.lexical.search("community", 50).unwrap().len(), n);
}

#[test]
fn fusion_defaults_follow_the_embedder_and_settings_override_them() {
    assert_eq!(FusionWeights::for_model(None), FusionWeights::default());
    assert_eq!(
        FusionWeights::for_model(Some("model2vec:minishlab/potion-code-16M-v2")).dense,
        0.5
    );
    assert_eq!(
        FusionWeights::for_model(Some("candle:jinaai/jina-embeddings-v2-base-code")).lexical,
        1.5
    );
    let cfg = FusionConfig {
        dense: Some(2.0),
        ..FusionConfig::default()
    };
    let w = cfg.resolve(FusionWeights::for_model(Some("model2vec:x")));
    assert_eq!((w.lexical, w.dense), (1.0, 2.0));
}

fn corpus_search(e: &Engine, query: &str, corpus: CorpusFilter, mode: Mode) -> Vec<Hit> {
    e.search(
        query,
        &SearchOpts {
            corpus,
            mode,
            ..SearchOpts::default()
        },
    )
    .unwrap()
}

#[test]
fn code_and_docs_corpora_stay_apart() {
    let dir = calls_project();
    let e = engine(dir.path());
    for mode in [Mode::Hybrid, Mode::Lexical, Mode::Dense] {
        let code = corpus_search(&e, "project", CorpusFilter::Code, mode);
        assert!(!code.is_empty(), "{mode:?}");
        assert!(
            code.iter().all(|h| !h.path.ends_with(".md")),
            "{mode:?}: {code:?}"
        );
        let docs = corpus_search(&e, "project", CorpusFilter::Docs, mode);
        assert!(!docs.is_empty(), "{mode:?}");
        assert!(docs.iter().all(|h| h.path == "README.md"), "{mode:?}: {docs:?}");
    }
    let all = corpus_search(&e, "project", CorpusFilter::All, Mode::Hybrid);
    assert!(all.iter().any(|h| h.path == "README.md"));
    assert!(all.iter().any(|h| h.path.ends_with(".go")));
}

#[test]
fn context_packs_follow_the_corpus() {
    let dir = calls_project();
    let e = engine(dir.path());
    let code = e
        .context_for("Start helper process", 2000, CorpusFilter::Code)
        .unwrap();
    let paths: Vec<&str> = code.spans.iter().map(|s| s.path.as_str()).collect();
    assert!(
        paths.contains(&"pkg/sidecar/sidecar.go"),
        "the callee is pulled in: {paths:?}"
    );
    assert!(!paths.contains(&"README.md"));

    let docs = e
        .context_for("project used in tests", 2000, CorpusFilter::Docs)
        .unwrap();
    assert!(!docs.spans.is_empty());
    assert!(docs.spans.iter().all(|s| s.path == "README.md"));
    assert_eq!(docs.timings.expand_ms, 0, "documentation is not expanded");

    let mut all = e
        .context_for("Start helper process", 2000, CorpusFilter::All)
        .unwrap();
    let mut plain = e.context("Start helper process", 2000).unwrap();
    all.timings = Timings::default();
    plain.timings = Timings::default();
    assert_eq!(all, plain, "context() searches both corpora");
}

#[test]
fn identical_text_is_returned_once_with_its_copies() {
    let dir = calls_project();
    let body = "package vendored\n\n// LocateBinary finds a sidecar executable.\nfunc LocateBinary(name string) string { return name }\n";
    fs::create_dir_all(dir.path().join("third_party")).unwrap();
    fs::write(dir.path().join("pkg/sidecar/copy.go"), body).unwrap();
    fs::write(dir.path().join("third_party/copy.go"), body).unwrap();
    let e = engine(dir.path());
    let hits = e.search("LocateBinary vendored", &SearchOpts::default()).unwrap();
    let copies: Vec<&Hit> = hits.iter().filter(|h| h.path.ends_with("copy.go")).collect();
    assert_eq!(copies.len(), 1, "{hits:?}");
    assert_eq!(copies[0].duplicates.len(), 1);
    let pack = e.context("LocateBinary vendored", 4000).unwrap();
    let copies: Vec<&Hit> = pack
        .spans
        .iter()
        .filter(|h| h.path.ends_with("copy.go"))
        .collect();
    assert_eq!(
        copies.len(),
        1,
        "a context pack reads the text once too: {:?}",
        pack.spans
    );
    assert_eq!(copies[0].duplicates.len(), 1);
}

#[test]
fn outlines_keep_the_doc_comment_and_declaration() {
    assert_eq!(
        signature("\n// Start launches a helper.\nfunc Start(name string) error {\n\treturn run(name)\n}\n"),
        "// Start launches a helper.\nfunc Start(name string) error {"
    );
    assert_eq!(
        signature("---\ntitle: x\n---\n# Guide\n\nFirst line."),
        "title: x\n# Guide"
    );
    let long = "x".repeat(400);
    assert_eq!(signature(&long).chars().count(), 161);
}
