use super::*;
use crate::rpc::handle_line;
use loom_rag::embed::Backend;
use loom_rag::testutil::project;
use rstest::{fixture, rstest};

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

#[fixture]
fn lexical_server() -> Server {
    let mut cfg = Config::default();
    cfg.embed.backend = Backend::None;
    Server::new(cfg)
}

#[rstest]
fn an_idle_engine_is_closed_and_releases_the_project_for_another_process(
    lexical_server: Server,
    #[from(lexical_server)] second_process: Server,
) {
    let dir = project();
    let root = dir.path().to_str().unwrap();
    let mut holder = lexical_server.with_timing(Some(Duration::from_millis(30)), Duration::ZERO);
    assert_eq!(
        call(&mut holder, "index", json!({"root": root}))["result"]["files_scanned"],
        3
    );

    // While the holder keeps the engine open, a second process cannot take the project.
    let mut other = second_process.with_timing(None, Duration::ZERO);
    let r = call(&mut other, "status", json!({"root": root}));
    assert!(r["error"].is_object(), "the project was not locked: {r}");

    // Not idle long enough yet: the engine stays.
    holder.idle();
    assert!(!holder.engines.is_empty());

    std::thread::sleep(Duration::from_millis(60));
    holder.idle();
    assert!(holder.engines.is_empty(), "the idle engine was not closed");
    let r = call(&mut other, "status", json!({"root": root}));
    assert!(
        r["result"]["files"].as_u64().unwrap() >= 3,
        "the released project could not be opened: {r}"
    );

    // The holder reopens on demand and keeps the index it built.
    drop(other);
    assert_eq!(
        call(&mut holder, "status", json!({"root": root}))["result"]["files"],
        3
    );
}

#[rstest]
fn opening_a_locked_project_waits_for_the_holder_to_release_it(
    lexical_server: Server,
    #[from(lexical_server)] waiting_process: Server,
) {
    let dir = project();
    let root = dir.path().to_str().unwrap().to_string();
    let mut holder = lexical_server.with_timing(Some(Duration::from_millis(20)), Duration::ZERO);
    call(&mut holder, "index", json!({"root": root}));

    let waiter_root = root.clone();
    let waiter = std::thread::spawn(move || {
        let mut other = waiting_process.with_timing(None, Duration::from_secs(10));
        let started = Instant::now();
        let r = call(&mut other, "status", json!({"root": waiter_root}));
        (r, started.elapsed())
    });

    // The holder's idle tick (as the serve loop would run it) lets the waiter in.
    std::thread::sleep(Duration::from_millis(400));
    holder.idle();
    let (r, waited) = waiter.join().unwrap();
    assert!(
        r["result"].is_object(),
        "the waiter failed instead of waiting: {r}"
    );
    assert!(
        waited >= Duration::from_millis(300),
        "the waiter did not wait ({waited:?})"
    );
}

#[rstest]
fn engines_are_kept_when_idle_closing_is_disabled(lexical_server: Server) {
    let dir = project();
    let root = dir.path().to_str().unwrap();
    let mut s = lexical_server.with_timing(None, Duration::ZERO);
    call(&mut s, "index", json!({"root": root}));
    std::thread::sleep(Duration::from_millis(20));
    s.idle();
    assert_eq!(s.engines.len(), 1);
}

#[test]
fn quality_loc_reports_production_lines_against_the_directives() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let body: String = (0..12).map(|i| format!("fn f{i}() {{}}\n")).collect();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        format!("{body}#[cfg(test)]\nmod tests {{ fn t() {{}} }}\n"),
    )
    .unwrap();
    let mut s = Server::new(Config::default());

    let r = call(&mut s, "quality.loc", json!({"root": root}));
    assert_eq!(r["result"]["totals"]["failures"], 0);
    assert_eq!(r["result"]["files"][0]["production"], 12);

    let r = call(
        &mut s,
        "quality.loc",
        json!({"root": root, "max_production_lines": 10, "warn_at": 8}),
    );
    assert_eq!(r["result"]["totals"]["failures"], 1);
    assert_eq!(r["result"]["files"][0]["status"], "fail");

    let r = call(
        &mut s,
        "quality.loc",
        json!({"root": root, "exclude": ["src/**"], "max_production_lines": 10, "warn_at": 8}),
    );
    assert_eq!(r["result"]["totals"]["files"], 0);

    let r = call(&mut s, "quality.loc", json!({"root": root, "warn_at": 900}));
    assert_eq!(
        r["error"]["code"],
        code::INVALID_PARAMS,
        "warn_at above the maximum"
    );
    std::fs::create_dir_all(dir.path().join(".github/dendrite")).unwrap();
    std::fs::write(dir.path().join(".github/dendrite/directives.toml"), "[loc\n").unwrap();
    let r = call(&mut s, "quality.loc", json!({"root": root}));
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    let r = call(&mut s, "quality.loc", json!({"nothing": 1}));
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS, "root is required");
}

#[test]
fn quality_clones_and_rstest_report_through_the_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let fun = |name: &str, k: i32| {
        format!(
            "fn {name}(items: &[i32]) -> i32 {{\n    let mut total = {k};\n    for item in items {{\n        if *item > {k} {{\n            total += item * 2;\n        }}\n    }}\n    total\n}}\n"
        )
    };
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), fun("alpha", 1)).unwrap();
    std::fs::write(dir.path().join("src/b.rs"), fun("beta", 2)).unwrap();
    let mut s = Server::new(Config::default());

    let r = call(&mut s, "quality.clones", json!({"root": root, "min_tokens": 10}));
    assert_eq!(r["result"]["totals"]["groups"], 1);
    assert_eq!(r["result"]["groups"][0]["locations"][0]["path"], "src/a.rs");
    let r = call(
        &mut s,
        "quality.clones",
        json!({"root": root, "min_tokens": 10, "max_percent": 100.0, "include_tests": false, "min_lines": 5, "exclude": ["src/b.rs"]}),
    );
    assert_eq!(r["result"]["totals"]["groups"], 0);
    let r = call(&mut s, "quality.clones", json!({"root": root, "min_tokens": 1}));
    assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    std::fs::create_dir_all(dir.path().join(".github/dendrite")).unwrap();
    std::fs::write(
        dir.path().join(".github/dendrite/directives.toml"),
        "[duplication\n",
    )
    .unwrap();
    assert_eq!(
        call(&mut s, "quality.clones", json!({"root": root}))["error"]["code"],
        code::INVALID_PARAMS
    );

    let r = call(&mut s, "quality.rstest", json!({"root": root}));
    assert_eq!(r["result"]["files"], 2);
    assert_eq!(r["result"]["violations"].as_array().unwrap().len(), 0);
    let r = call(
        &mut s,
        "quality.rstest",
        json!({"root": root, "exclude": ["src/a.rs"]}),
    );
    assert_eq!(r["result"]["files"], 1);
    assert_eq!(
        call(&mut s, "quality.rstest", json!({"nothing": 1}))["error"]["code"],
        code::INVALID_PARAMS
    );
}
