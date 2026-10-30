//! Optional LLM enrichment tests (community summaries and concepts).

use super::*;

/// An engine over `root` that asks the model served at `url` for summaries and concepts when `enabled`.
fn llm_engine(root: &std::path::Path, enabled: bool, url: &str) -> Engine {
    let cfg = Config {
        graph_llm: crate::config::GraphLlmSettings {
            enabled,
            model: Some("test-model".into()),
            summaries: true,
            concepts: true,
        },
        ..Config::default()
    };
    Engine::open_with_embedder(root, cfg, None)
        .unwrap()
        .with_llm(crate::llm::Client::new(
            Some(url.to_string()),
            None,
            "test-model".into(),
        ))
}

#[test]
fn llm_request_shape_and_enrichment() {
    use crate::graph::query::Explanation;
    let dir = project();
    fs::write(
        dir.path().join("pkg/sidecar/client.go"),
        "package sidecar\n\nfunc Start(name string) string { return LocateBinary(name) }\n",
    )
    .unwrap();
    let resp = r#"{"choices":[{"message":{"content":"[{\"name\": \"Testing\", \"description\": \"Test suite concept\"}]"}}]}"#;
    let stub = crate::testutil::HttpStub::serve(resp);
    let cfg = Config {
        graph_llm: crate::config::GraphLlmSettings {
            enabled: true,
            model: Some("test-model".into()),
            summaries: true,
            concepts: true,
        },
        ..Config::default()
    };
    let mut e = Engine::open_with_embedder(dir.path(), cfg, None)
        .unwrap()
        .with_llm(crate::llm::Client::new(
            Some(stub.url.clone()),
            Some("secret-key".into()),
            "test-model".into(),
        ));
    let r = e.index(false).unwrap();
    assert_eq!(r.llm_error, None);

    // Check captured requests
    assert!(stub.request_count() >= 1);
    let req = stub.request();
    assert!(
        req.head
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-key")
    );
    assert_eq!(req.body["model"], "test-model");
    assert_eq!(req.body["temperature"], 0.0);

    // Verify concepts were extracted and exposed on graph_explain
    let expl = e.graph_explain("README.md").unwrap();
    assert!(!expl.is_empty());
    let Explanation::File { concepts, .. } = &expl[0] else {
        panic!("expected File explanation");
    };
    assert_eq!(concepts.len(), 1);
    assert_eq!(concepts[0].name, "Testing");
    assert_eq!(concepts[0].description, "Test suite concept");

    // Verify summary chunk has the appended text
    let mut found_summary = false;
    e.ledger
        .for_each_chunk(&mut |c| {
            if c.path.starts_with("@community/") {
                found_summary = true;
                assert!(c.content.contains("Testing"));
            }
        })
        .unwrap();
    assert!(found_summary);
}

#[test]
fn llm_caching_makes_no_requests_on_second_run() {
    let dir = project();
    let resp = r#"{"choices":[{"message":{"content":"[{\"name\": \"Testing\", \"description\": \"Test suite concept\"}]"}}]}"#;
    let stub = crate::testutil::HttpStub::serve(resp);
    let mut e = llm_engine(dir.path(), true, &stub.url);
    e.index(false).unwrap();
    let initial_count = stub.request_count();
    assert!(initial_count > 0);

    // Second index run: unchanged files and communities
    let r2 = e.index(false).unwrap();
    assert_eq!(r2.llm_error, None);
    assert_eq!(
        stub.request_count(),
        initial_count,
        "second index run must make no requests"
    );

    // Full rebuild: llm_cache persists in redb, so no requests
    let r3 = e.index(true).unwrap();
    assert_eq!(r3.llm_error, None);
    assert_eq!(
        stub.request_count(),
        initial_count,
        "full index rebuild must reuse llm_cache and make no requests"
    );
}

#[test]
fn llm_failure_keeps_deterministic_summary() {
    let dir = project();
    // A second file calling into the first makes a community of two, with a summary.
    fs::write(
        dir.path().join("pkg/sidecar/client.go"),
        "package sidecar\n\nfunc Start(name string) string { return LocateBinary(name) }\n",
    )
    .unwrap();
    let stub =
        crate::testutil::HttpStub::serve_status(500, "Internal Server Error", r#"{"error": "overloaded"}"#);
    let mut e = llm_engine(dir.path(), true, &stub.url);
    let r = e.index(false).unwrap();
    assert!(r.llm_error.is_some(), "must record llm_error");

    // The deterministic summary chunk is preserved
    let mut found_summary = false;
    e.ledger
        .for_each_chunk(&mut |c| {
            if c.path.starts_with("@community/") {
                found_summary = true;
                assert!(c.content.contains("Files:"));
            }
        })
        .unwrap();
    assert!(found_summary);
}

#[test]
fn llm_disabled_makes_no_requests() {
    let dir = project();
    let stub = crate::testutil::HttpStub::serve(r#"{"choices":[]}"#);
    let mut e = llm_engine(dir.path(), false, &stub.url);
    let r = e.index(false).unwrap();
    assert_eq!(r.llm_error, None);
    assert_eq!(stub.request_count(), 0);
}

#[test]
fn llm_config_validation() {
    let dir = project();
    let cfg = Config {
        graph_llm: crate::config::GraphLlmSettings {
            enabled: true,
            model: None,
            summaries: true,
            concepts: true,
        },
        ..Config::default()
    };
    assert!(Engine::open(dir.path(), cfg).is_err());
}
