use super::*;
use crate::engine::Config;
use std::fs;

fn engine() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("go.mod", "module example.com/m\n"),
        (
            "pkg/sidecar/sidecar.go",
            "package sidecar\n\nfunc LocateBinary(n string) string { return n }\n",
        ),
        (
            "pkg/sidecar/client.go",
            "package sidecar\n\nfunc Start(n string) { run(LocateBinary(n)) }\n\nfunc run(p string) {}\n",
        ),
        (
            "pkg/rag/retriever.go",
            "package rag\n\nimport \"example.com/m/pkg/sidecar\"\n\nfunc Retrieve() { sidecar.Start(\"x\") }\n",
        ),
    ];
    for (p, src) in files {
        let path = dir.path().join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, src).unwrap();
    }
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
    e.index(false).unwrap();
    (dir, e)
}

#[test]
fn path_follows_calls_forward_then_backward() {
    let (_d, e) = engine();
    let p = e.graph_path("Retrieve", "LocateBinary", &[], 6).unwrap();
    assert!(p.found && p.directed);
    let names: Vec<&str> = p.steps.iter().map(|s| s.node.as_str()).collect();
    assert_eq!(names, ["Retrieve", "Start", "LocateBinary"]);
    assert_eq!(p.steps[1].via, Some(EdgeKind::Call));

    let back = e.graph_path("LocateBinary", "Retrieve", &[], 6).unwrap();
    assert!(back.found && !back.directed);
    assert!(back.steps[1].reversed);

    let files = e
        .graph_path("pkg/rag/retriever.go", "pkg/sidecar/sidecar.go", &[], 6)
        .unwrap();
    assert_eq!(files.level, "file");
    assert!(files.found && files.directed);

    assert!(!e.graph_path("Retrieve", "LocateBinary", &[], 1).unwrap().found);
    assert!(
        !e.graph_path("Retrieve", "LocateBinary", &[EdgeKind::Type], 6)
            .unwrap()
            .found
    );
}

#[test]
fn explains_symbols_and_files() {
    let (_d, e) = engine();
    let x = e.graph_explain("Start").unwrap();
    let Explanation::Symbol {
        users,
        uses,
        community,
        ..
    } = &x[0]
    else {
        panic!("expected a symbol: {x:?}");
    };
    assert_eq!(users[0].symbol, "Retrieve");
    assert!(uses.iter().any(|n| n.symbol == "LocateBinary"));
    assert!(community.is_some());

    let x = e.graph_explain("pkg/rag/retriever.go").unwrap();
    let Explanation::File {
        imports, depends_on, ..
    } = &x[0]
    else {
        panic!("expected a file: {x:?}");
    };
    assert!(imports.contains(&"pkg/sidecar/client.go".to_string()));
    assert!(depends_on.contains(&"pkg/sidecar/client.go".to_string()));
    let x = e.graph_explain("pkg/sidecar/client.go").unwrap();
    let Explanation::File {
        imported_by, used_by, ..
    } = &x[0]
    else {
        panic!();
    };
    assert_eq!(imported_by, &["pkg/rag/retriever.go"]);
    assert_eq!(used_by, &["pkg/rag/retriever.go"]);
    assert!(e.graph_explain("Nope").unwrap().is_empty());
}

#[test]
fn communities_and_god_nodes() {
    let (_d, e) = engine();
    let cs = e.graph_communities(10, 2).unwrap();
    assert!(!cs.is_empty());
    assert!(cs[0].summary.starts_with("Community "));
    assert!(cs.iter().all(|c| c.size >= 2));
    let gods = e.graph_god_nodes(1, 10).unwrap();
    assert!(
        gods.iter()
            .any(|g| g.symbol == "LocateBinary" && g.in_degree >= 1)
    );
    assert!(e.graph_god_nodes(GOD_NODE_IN_DEGREE, 10).unwrap().is_empty());
}
