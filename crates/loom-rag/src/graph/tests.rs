use super::*;
use crate::chunk::{ChunkOptions, analyze_file};

pub(crate) fn graph_of(sources: &[(&str, &str)], go_mod: Option<&str>) -> CodeGraph {
    let mut chunks = Vec::new();
    let mut files = BTreeMap::new();
    for (path, src) in sources {
        let lang = Lang::from_path(std::path::Path::new(path)).unwrap();
        let (c, imports) = analyze_file(path, lang, src, ChunkOptions::default());
        files.insert(
            path.to_string(),
            FileEntry {
                hash: String::new(),
                size: 0,
                mtime_ns: 0,
                chunk_ids: c.iter().map(|c| c.id.clone()).collect(),
                imports,
            },
        );
        chunks.extend(c);
    }
    let mods = go_mod.map(|m| ("go.mod".to_string(), format!("module {m}\n")));
    CodeGraph::build(chunks, &files, mods)
}

#[test]
fn links_files_by_calls_imports_and_types() {
    let g = graph_of(
        &[
            (
                "pkg/sidecar/sidecar.go",
                "package sidecar\n\ntype Client struct{}\n\nfunc LocateBinary(n string) string { return n }\n",
            ),
            (
                "pkg/rag/retriever.go",
                "package rag\n\nimport \"example.com/m/pkg/sidecar\"\n\nfunc Run(c *sidecar.Client) { sidecar.LocateBinary(\"x\") }\n",
            ),
        ],
        Some("example.com/m"),
    );
    let from = g.file_id("pkg/rag/retriever.go").unwrap();
    let to = g.file_id("pkg/sidecar/sidecar.go").unwrap();
    let kinds: Vec<EdgeKind> = g
        .file_edges(from)
        .filter(|(t, _, _)| *t == to)
        .map(|(_, k, _)| k)
        .collect();
    assert_eq!(kinds, [EdgeKind::Call, EdgeKind::Import, EdgeKind::Type]);
    assert_eq!(g.file_edges(to).count(), 0);

    let locate = g.symbols_named("LocateBinary")[0];
    let run = g.symbols_named("Run")[0];
    assert_eq!(g.users(locate).collect::<Vec<_>>(), [(run, EdgeKind::Call)]);
    assert_eq!(g.in_degree(locate), 1);
    assert_eq!(g.sites(locate)[0].path, "pkg/sidecar/sidecar.go");

    let und = g.undirected();
    assert_eq!(und[to as usize].len(), 1);
    assert!((und[to as usize][0].1 - 1.9).abs() < 1e-5);
}

#[test]
fn names_link_only_to_visible_definitions() {
    let g = graph_of(
        &[
            (
                "pkg/clock/clock.go",
                "package clock\n\nfunc Now() int { return 1 }\n",
            ),
            ("pkg/clock/clock_test.go", "package clock\n\nfunc Helper() {}\n"),
            (
                "pkg/app/app.go",
                "package app\n\nimport \"time\"\n\nfunc Run() { time.Now(); Helper() }\n",
            ),
        ],
        Some("example.com/m"),
    );
    // `time.Now` is not the project's `clock.Now`: app does not import clock.
    assert_eq!(g.in_degree(g.symbols_named("Now")[0]), 0);
    assert_eq!(g.in_degree(g.symbols_named("Helper")[0]), 0);
}

#[test]
fn unresolved_imports_are_kept_as_external() {
    let g = graph_of(
        &[(
            "a.go",
            "package a\n\nimport \"fmt\"\n\nfunc F() { fmt.Println() }\n",
        )],
        None,
    );
    assert_eq!(g.external_imports(g.file_id("a.go").unwrap()), ["fmt"]);
}
