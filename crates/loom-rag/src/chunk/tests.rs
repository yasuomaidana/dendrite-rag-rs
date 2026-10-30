use super::*;
use rstest::{fixture, rstest};

const SMALL: ChunkOptions = ChunkOptions {
    max_chars: 120,
    min_chars: 30,
};

#[fixture]
fn go_src() -> String {
    let mut s = String::from("package demo\n\nimport \"fmt\"\n\n");
    s.push_str("// Locate finds the binary.\nfunc (l Locator) Locate(name string) string {\n\treturn fmt.Sprint(name)\n}\n\n");
    s.push_str("func Big() {\n");
    for i in 0..20 {
        s.push_str(&format!(
            "\tif x{i} := compute({i}); x{i} > 0 {{\n\t\tfmt.Println(x{i})\n\t}}\n"
        ));
    }
    s.push_str("}\n\nfunc tiny() {}\n");
    s
}

#[rstest]
fn chunks_tile_the_file_and_respect_the_budget(go_src: String) {
    let src = go_src;
    let chunks = chunk_file("pkg/demo/demo.go", Lang::Go, &src, SMALL);
    assert!(chunks.len() > 3, "expected several chunks, got {}", chunks.len());
    for c in &chunks {
        assert!(
            nonws(c.content.as_bytes()) <= SMALL.max_chars + 40,
            "oversized chunk: {}",
            c.content
        );
        assert_eq!(&src[c.start_byte..c.end_byte], c.content);
    }
    // Every non-blank line is covered exactly once.
    let mut covered = vec![0; src.lines().count() + 1];
    for c in &chunks {
        for n in &mut covered[c.start_line..=c.end_line] {
            *n += 1;
        }
    }
    for (i, line) in src.lines().enumerate() {
        if !line.trim().is_empty() {
            assert_eq!(
                covered[i + 1],
                1,
                "line {} covered {} times: {line}",
                i + 1,
                covered[i + 1]
            );
        }
    }
}

#[rstest]
fn doc_comments_travel_with_their_function_and_symbols_are_tagged(go_src: String) {
    let src = go_src;
    let chunks = chunk_file("demo.go", Lang::Go, &src, ChunkOptions::default());
    let locate = chunks
        .iter()
        .find(|c| c.symbols.contains(&"Locate".to_string()))
        .unwrap();
    assert!(locate.content.contains("// Locate finds the binary."));
    assert!(chunks.iter().any(|c| c.refs.contains(&"Sprint".to_string())));
}

#[rstest]
fn large_functions_are_split_inside_and_keep_their_scope(go_src: String) {
    let src = go_src;
    let chunks = chunk_file("demo.go", Lang::Go, &src, SMALL);
    let inner: Vec<_> = chunks
        .iter()
        .filter(|c| c.scope.iter().any(|s| s.ends_with("Big")))
        .collect();
    assert!(inner.len() >= 2, "Big should be split into several scoped chunks");
    assert!(inner[0].indexed_text().contains("// scope: function Big"));
}

#[test]
fn rust_methods_get_impl_scope() {
    let mut src = String::from("struct Engine;\n\nimpl Engine {\n");
    for i in 0..6 {
        src.push_str(&format!(
            "    pub fn method_{i}(&self, value: usize) -> usize {{\n        let doubled = value * 2 + {i};\n        doubled.saturating_sub(1)\n    }}\n\n"
        ));
    }
    src.push_str("}\n");
    let chunks = chunk_file("src/engine.rs", Lang::Rust, &src, SMALL);
    let m = chunks
        .iter()
        .find(|c| c.symbols.contains(&"method_3".to_string()))
        .unwrap();
    assert_eq!(m.scope.first().map(String::as_str), Some("impl Engine"));
}

#[test]
fn python_and_typescript_parse() {
    let py = "class A:\n    def run(self):\n        return helper(1)\n\ndef helper(x):\n    return x\n";
    let chunks = chunk_file("a.py", Lang::Python, py, ChunkOptions::default());
    let syms: Vec<_> = chunks.iter().flat_map(|c| c.symbols.clone()).collect();
    assert!(
        syms.contains(&"A".into()) && syms.contains(&"helper".into()),
        "{syms:?}"
    );

    let ts = "export interface Opts { k: number }\nexport function search(o: Opts): number { return o.k }\n";
    let chunks = chunk_file("a.ts", Lang::TypeScript, ts, ChunkOptions::default());
    let syms: Vec<_> = chunks.iter().flat_map(|c| c.symbols.clone()).collect();
    assert!(syms.contains(&"search".into()), "{syms:?}");
}

#[rstest]
#[case(
    "a.go",
    Lang::Go,
    "package a\n\nimport (\n\t\"fmt\"\n\tx \"example.com/m/pkg/x\"\n)\n\nfunc F() { fmt.Println(x.Run()) }\n",
    &["fmt", "example.com/m/pkg/x"][..],
    "Run"
)]
#[case(
    "src/lib.rs",
    Lang::Rust,
    "mod engine;\nuse crate::ledger::{Ledger, FileEntry};\n\nfn f() { open(); }\n",
    &["mod:engine", "crate::ledger::{Ledger, FileEntry}"][..],
    "open"
)]
#[case(
    "a.py",
    Lang::Python,
    "import os.path\nfrom .util import helper\n\ndef f():\n    return helper(1)\n",
    &["os.path", ".util"][..],
    "helper"
)]
#[case(
    "a.ts",
    Lang::TypeScript,
    "import { b } from './b';\nconst c = require('./c');\nexport function f() { return b(); }\n",
    &["./b", "./c"][..],
    "b"
)]
#[case(
    "A.java",
    Lang::Java,
    "import com.acme.util.Strings;\nclass A { void f() { Strings.trim(\"x\"); } }\n",
    &["com.acme.util.Strings"][..],
    "trim"
)]
#[case(
    "a.c",
    Lang::C,
    "#include \"util.h\"\n#include <stdio.h>\nint f(void) { return helper(1); }\n",
    &["util.h", "stdio.h"][..],
    "helper"
)]
#[case(
    "a.cpp",
    Lang::Cpp,
    "#include \"util.hpp\"\nint f() { return ns::helper(1); }\n",
    &["util.hpp"][..],
    "helper"
)]
fn calls_and_imports_are_extracted_per_language(
    #[case] path: &str,
    #[case] lang: Lang,
    #[case] src: &str,
    #[case] want: &[&str],
    #[case] call: &str,
) {
    let (chunks, imports) = analyze_file(path, lang, src, ChunkOptions::default());
    assert_eq!(imports, want, "{path}");
    assert!(
        chunks.iter().any(|c| c.calls.iter().any(|n| n == call)),
        "{path}: no call to {call} in {:?}",
        chunks.iter().map(|c| &c.calls).collect::<Vec<_>>()
    );
}

#[test]
fn markdown_splits_by_heading_and_keeps_the_path() {
    let body = "word ".repeat(40);
    let md = format!(
        "# Guide\n\nintro {body}\n\n## Install\n\n{body}\n\n```sh\n# not a heading\n```\n\n## Use\n\n{body}\n"
    );
    let chunks = chunk_file("docs/guide.md", Lang::Markdown, &md, SMALL);
    assert!(
        chunks
            .iter()
            .any(|c| c.scope == vec!["Guide".to_string(), "Install".to_string()])
    );
    assert!(
        chunks
            .iter()
            .all(|c| !c.scope.contains(&"not a heading".to_string()))
    );
}

#[test]
fn long_lines_are_cut() {
    let src = "x".repeat(1000);
    let chunks = chunk_file("data.json", Lang::Text, &src, SMALL);
    assert!(chunks.len() >= 8);
    assert_eq!(chunks.iter().map(|c| c.content.len()).sum::<usize>(), 1000);
}

#[test]
fn ids_change_with_content_and_hashes_with_text_only() {
    let a = chunk_file(
        "a.go",
        Lang::Go,
        "package a\n\nfunc F() {}\n",
        ChunkOptions::default(),
    );
    let b = chunk_file(
        "b.go",
        Lang::Go,
        "package a\n\nfunc F() {}\n",
        ChunkOptions::default(),
    );
    assert_ne!(a[0].id, b[0].id);
    assert_eq!(a[0].content_hash, b[0].content_hash);
}
