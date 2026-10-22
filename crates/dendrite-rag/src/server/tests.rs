use super::*;
use crate::rpc::handle_line;
use loom_rag::embed::Backend;
use loom_rag::testutil::project;

fn call(s: &mut Server, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    serde_json::from_str(&handle_line(s, &line).unwrap()).unwrap()
}

#[test]
fn full_method_table() {
    let dir = project();
    let root = dir.path().to_str().unwrap();
    let mut cfg = Config::default();
    cfg.embed.backend = Backend::None;
    let mut s = Server::new(cfg);

    let r = call(&mut s, "search", json!({"root": root, "query": "x"}));
    assert_eq!(r["error"]["data"]["kind"], "not_indexed");
    let r = call(&mut s, "graph.communities", json!({"root": root}));
    assert_eq!(r["error"]["data"]["kind"], "not_indexed");

    let r = call(&mut s, "index", json!({"root": root}));
    assert_eq!(r["result"]["files_scanned"], 3);
    let r = call(
        &mut s,
        "search",
        json!({"root": root, "query": "LocateBinary", "top_k": 2}),
    );
    assert_eq!(r["result"]["hits"][0]["path"], "pkg/sidecar/sidecar.go");
    let r = call(
        &mut s,
        "search",
        json!({"root": root, "query": "x", "mode": "bogus"}),
    );
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(
        &mut s,
        "context",
        json!({"root": root, "query": "LocateBinary", "token_budget": 500}),
    );
    assert!(r["result"]["tokens"].as_u64().unwrap() <= 500);
    let r = call(
        &mut s,
        "context",
        json!({"root": root, "query": "LocateBinary", "token_budget": 500, "docs_share": 1.0}),
    );
    assert!(r["result"]["tokens"].as_u64().unwrap() <= 500);
    let r = call(
        &mut s,
        "context",
        json!({"root": root, "query": "LocateBinary", "docs_share": 1.5}),
    );
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(
        &mut s,
        "ask",
        json!({"root": root, "question": "LocateBinary", "docs_share": -1}),
    );
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(
        &mut s,
        "symbols.definitions",
        json!({"root": root, "name": "IndexProject"}),
    );
    assert_eq!(r["result"]["hits"][0]["path"], "pkg/rag/indexer.go");
    let r = call(
        &mut s,
        "rank",
        json!({"root": root, "query": "sandbox browser", "documents": [
            {"id": "a", "text": "git commits"}, {"id": "b", "text": "browser sandbox"}]}),
    );
    assert_eq!(r["result"]["ranked"][0]["id"], "b");
    let r = call(&mut s, "graph.communities", json!({"root": root, "min_size": 1}));
    assert!(!r["result"]["communities"].as_array().unwrap().is_empty());
    let r = call(
        &mut s,
        "graph.god_nodes",
        json!({"root": root, "min_in_degree": 1}),
    );
    assert!(r["result"]["nodes"].is_array());
    let r = call(
        &mut s,
        "graph.path",
        json!({"root": root, "from": "pkg/rag/indexer.go", "to": "pkg/sidecar/sidecar.go", "kinds": ["call"]}),
    );
    assert_eq!(r["result"]["level"], "file");
    let r = call(
        &mut s,
        "graph.path",
        json!({"root": root, "from": "a", "to": "b", "kinds": ["bogus"]}),
    );
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(
        &mut s,
        "graph.explain",
        json!({"root": root, "target": "IndexProject"}),
    );
    assert_eq!(r["result"]["matches"][0]["kind"], "symbol");
    let r = call(&mut s, "grep", json!({"root": root, "pattern": "^func"}));
    assert_eq!(r["result"]["matches"].as_array().unwrap().len(), 2);
    let r = call(&mut s, "grep", json!({"root": root, "pattern": "("}));
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(&mut s, "status", json!({"root": root}));
    assert_eq!(r["result"]["files"], 3);
    let r = call(&mut s, "purge", json!({"root": root}));
    assert_eq!(r["result"]["removed"], true);
    assert!(!dir.path().join(".dendrite/rag").exists());

    let r = call(&mut s, "status", json!({"root": "/definitely/missing"}));
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(&mut s, "status", json!({}));
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
}
