//! cAST chunking over tree-sitter syntax trees, plus Markdown and text splitters
//! (`docs/specs/chunking.md`).

use std::ops::Range;

use serde::{Deserialize, Serialize};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, QueryCursor};

use crate::lang::Lang;

/// Chunk size budget, in non-whitespace characters.
#[derive(Debug, Clone, Copy)]
pub struct ChunkOptions {
    pub max_chars: usize,
    pub min_chars: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_chars: 1500,
            min_chars: 200,
        }
    }
}

/// A retrievable span of a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// `blake3(path ‖ 0 ‖ start ‖ 0 ‖ end ‖ 0 ‖ content)`, first 32 hex chars.
    pub id: String,
    /// `blake3(content)`, first 32 hex chars: the embedding cache key.
    pub content_hash: String,
    pub path: String,
    pub lang: Lang,
    pub start_byte: usize,
    pub end_byte: usize,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// Enclosing named scopes, outermost first, e.g. `["impl Engine", "method search"]`.
    pub scope: Vec<String>,
    /// Names defined in the chunk.
    pub symbols: Vec<String>,
    /// Names referenced (called, used as types) in the chunk.
    pub refs: Vec<String>,
    /// The subset of `refs` that are calls.
    #[serde(default)]
    pub calls: Vec<String>,
    /// The subset of `calls` made through a receiver or qualifier (`x.len()`, `pkg.F()`).
    #[serde(default)]
    pub method_calls: Vec<String>,
    pub content: String,
}

impl Chunk {
    /// The contextual header prepended for indexing (never returned in results).
    pub fn header(&self) -> String {
        let mut h = format!("// path: {}\n", self.path);
        if !self.scope.is_empty() {
            h.push_str("// scope: ");
            h.push_str(&self.scope.join(" › "));
            h.push('\n');
        }
        h
    }

    /// Header plus content: the text BM25 and the embedder see.
    pub fn indexed_text(&self) -> String {
        format!("{}{}", self.header(), self.content)
    }
}

/// Splits one file into chunks.
pub fn chunk_file(path: &str, lang: Lang, src: &str, opts: ChunkOptions) -> Vec<Chunk> {
    analyze_file(path, lang, src, opts).0
}

/// Splits one file into chunks and lists its import specifiers (module paths as written,
/// unquoted; a Rust `mod name;` is reported as `mod:name`).
pub fn analyze_file(path: &str, lang: Lang, src: &str, opts: ChunkOptions) -> (Vec<Chunk>, Vec<String>) {
    let lines = LineIndex::new(src);
    let chunks = match lang {
        Lang::Markdown => {
            let sections = markdown_sections(src, opts);
            sections
                .into_iter()
                .filter_map(|(r, scope)| build(path, lang, src, &lines, r, scope, &[], &[]))
                .collect()
        }
        Lang::Text => pack_text(src, 0..src.len(), opts.max_chars)
            .into_iter()
            .filter_map(|r| build(path, lang, src, &lines, r, Vec::new(), &[], &[]))
            .collect(),
        _ => match chunk_code(path, lang, src, &lines, opts) {
            Some(done) => return done,
            None => chunk_file(path, Lang::Text, src, opts)
                .into_iter()
                .map(|c| relang(c, lang))
                .collect(),
        },
    };
    (chunks, Vec::new())
}

fn relang(mut c: Chunk, lang: Lang) -> Chunk {
    c.lang = lang;
    c
}

// ---------------------------------------------------------------------------------------------
// Code

#[derive(Debug, Clone, PartialEq, Eq)]
enum TagKind {
    Def(String),
    Ref(String),
}

#[derive(Debug, Clone)]
struct Tag {
    kind: TagKind,
    name: String,
    name_byte: usize,
    range: Range<usize>,
    /// The name is the member of a receiver or qualifier expression (`a.name`).
    member: bool,
}

fn chunk_code(
    path: &str,
    lang: Lang,
    src: &str,
    lines: &LineIndex,
    opts: ChunkOptions,
) -> Option<(Vec<Chunk>, Vec<String>)> {
    let mut parser = Parser::new();
    parser.set_language(&lang.grammar()?).ok()?;
    let tree = parser.parse(src, None)?;
    let root = tree.root_node();
    let bytes = src.as_bytes();

    let mut groups = Vec::new();
    cast(root, bytes, opts.max_chars, &mut groups);
    let spans = contiguous(&groups, src);
    let spans = merge_small(spans, bytes, opts);

    let tags = tags(lang, root, bytes);
    let defs: Vec<&Tag> = tags
        .iter()
        .filter(|t| matches!(t.kind, TagKind::Def(_)))
        .collect();
    let refs: Vec<&Tag> = tags
        .iter()
        .filter(|t| matches!(t.kind, TagKind::Ref(_)))
        .collect();

    let chunks = spans
        .into_iter()
        .filter_map(|r| {
            let sig = significant(bytes, r.clone())?;
            let mut scope: Vec<&Tag> = tags
                .iter()
                .filter(|t| is_scope(&t.kind) && t.range.start <= sig.start && t.range.end >= sig.end)
                .collect();
            scope.sort_by_key(|t| (t.range.start, std::cmp::Reverse(t.range.end)));
            scope.dedup_by_key(|t| t.range.clone());
            let scope = scope
                .iter()
                .map(|t| format!("{} {}", kind_label(&t.kind), t.name))
                .collect();
            build(path, lang, src, lines, r, scope, &defs, &refs)
        })
        .collect();
    Some((chunks, imports(lang, root, bytes)))
}

/// Import specifiers in source order, deduplicated, without quotes or angle brackets.
fn imports(lang: Lang, root: Node, src: &[u8]) -> Vec<String> {
    let Some(query) = lang.imports() else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, src);
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        for cap in m.captures() {
            let text = cap.node.utf8_text(src).unwrap_or_default();
            let spec = text.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '<' | '>'));
            match names[cap.index as usize] {
                "import" if !spec.is_empty() => out.push(spec.to_string()),
                "import.mod" => out.push(format!("mod:{spec}")),
                _ => {}
            }
        }
    }
    dedup_keep_order(&mut out);
    out
}

/// cAST: greedily pack sibling nodes up to `max` non-whitespace characters, recursing into
/// nodes that are too large on their own and splitting oversized leaves by lines.
fn cast(node: Node, src: &[u8], max: usize, out: &mut Vec<Range<usize>>) {
    let mut cur: Option<Range<usize>> = None;
    let mut cur_size = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let r = child.byte_range();
        let size = nonws(&src[r.clone()]);
        if size > max {
            out.extend(cur.take());
            cur_size = 0;
            if child.child_count() > 0 {
                cast(child, src, max, out);
            } else {
                out.extend(split_lines(src, r, max));
            }
        } else if cur_size + size > max {
            out.extend(cur.replace(r));
            cur_size = size;
        } else {
            cur = Some(cur.map_or(r.clone(), |c| c.start..r.end));
            cur_size += size;
        }
    }
    out.extend(cur);
}

/// Turns ordered node groups into spans that tile the whole file. Each boundary moves back
/// to the start of its line so indentation, doc comments and headers travel with the
/// following node.
fn contiguous(groups: &[Range<usize>], src: &str) -> Vec<Range<usize>> {
    let bytes = src.as_bytes();
    let mut bounds = vec![0];
    for g in groups.iter().skip(1) {
        let mut b = g.start;
        while b > 0 && bytes[b - 1] != b'\n' {
            b -= 1;
        }
        if b > *bounds.last().unwrap() {
            bounds.push(b);
        }
    }
    bounds.push(src.len());
    bounds
        .windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| w[0]..w[1])
        .collect()
}

/// Merges neighbouring spans when one of them is below `min` and the result fits in `max`.
fn merge_small(spans: Vec<Range<usize>>, src: &[u8], opts: ChunkOptions) -> Vec<Range<usize>> {
    let mut out: Vec<(Range<usize>, usize)> = Vec::new();
    for r in spans {
        let size = nonws(&src[r.clone()]);
        if let Some((last, last_size)) = out.last_mut()
            && (*last_size < opts.min_chars || size < opts.min_chars)
            && *last_size + size <= opts.max_chars
        {
            last.end = r.end;
            *last_size += size;
            continue;
        }
        out.push((r, size));
    }
    out.into_iter().map(|(r, _)| r).collect()
}

fn tags(lang: Lang, root: Node, src: &[u8]) -> Vec<Tag> {
    let Some(query) = lang.tags() else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, src);
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut kind = None;
        let mut range = None;
        for cap in m.captures() {
            let cname = names[cap.index as usize];
            if cname == "name" {
                let text = cap.node.utf8_text(src).unwrap_or_default();
                name = Some((text.to_string(), cap.node.start_byte(), is_member(cap.node)));
            } else if let Some(k) = cname.strip_prefix("definition.") {
                kind = Some(TagKind::Def(k.to_string()));
                range = Some(cap.node.byte_range());
            } else if let Some(k) = cname.strip_prefix("reference.") {
                kind = Some(TagKind::Ref(k.to_string()));
                range = Some(cap.node.byte_range());
            }
        }
        if let (Some((name, name_byte, member)), Some(kind), Some(range)) = (name, kind, range)
            && !name.is_empty()
        {
            out.push(Tag {
                kind,
                name,
                name_byte,
                range,
                member,
            });
        }
    }
    out
}

/// Whether a name node is the member part of `receiver.name` in any supported grammar.
fn is_member(name: Node) -> bool {
    let Some(parent) = name.parent() else {
        return false;
    };
    match parent.kind() {
        "field_expression" | "selector_expression" | "member_expression" | "attribute" => true,
        // Java: `obj.call()` carries an `object` field; a bare `call()` does not.
        "method_invocation" => parent.child_by_field_name("object").is_some(),
        _ => false,
    }
}

/// Definitions open a scope, and so do `impl` blocks (tagged as implementation references).
fn is_scope(kind: &TagKind) -> bool {
    match kind {
        TagKind::Def(_) => true,
        TagKind::Ref(k) => k == "implementation",
    }
}

fn kind_label(kind: &TagKind) -> &str {
    match kind {
        TagKind::Def(k) => k.as_str(),
        TagKind::Ref(_) => "impl",
    }
}

// ---------------------------------------------------------------------------------------------
// Markdown and text

/// Splits Markdown by headings outside code fences; returns spans with their heading path.
fn markdown_sections(src: &str, opts: ChunkOptions) -> Vec<(Range<usize>, Vec<String>)> {
    let bytes = src.as_bytes();
    let mut sections: Vec<(usize, Vec<String>)> = vec![(0, Vec::new())];
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut in_fence = false;
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && t.starts_with('#') {
            let level = t.bytes().take_while(|&b| b == b'#').count();
            let rest = &t[level..];
            if level <= 6 && (rest.starts_with(' ') || rest.trim().is_empty()) {
                let title = rest.trim().trim_end_matches('#').trim().to_string();
                stack.retain(|(l, _)| *l < level);
                stack.push((level, title));
                sections.push((pos, stack.iter().map(|(_, t)| t.clone()).collect()));
            }
        }
        pos += line.len();
    }

    let mut out: Vec<(Range<usize>, Vec<String>, usize)> = Vec::new();
    for (i, (start, scope)) in sections.iter().enumerate() {
        let end = sections.get(i + 1).map_or(src.len(), |s| s.0);
        if start >= &end {
            continue;
        }
        let size = nonws(&bytes[*start..end]);
        if size > opts.max_chars {
            for r in pack_text(src, *start..end, opts.max_chars) {
                let s = nonws(&bytes[r.clone()]);
                out.push((r, scope.clone(), s));
            }
            continue;
        }
        if let Some((last, _, last_size)) = out.last_mut()
            && (*last_size < opts.min_chars || size < opts.min_chars)
            && *last_size + size <= opts.max_chars
        {
            last.end = end;
            *last_size += size;
            continue;
        }
        out.push((*start..end, scope.clone(), size));
    }
    out.into_iter().map(|(r, s, _)| (r, s)).collect()
}

/// Packs blank-line-separated paragraphs of `range` into spans of at most `max`
/// non-whitespace characters; oversized paragraphs are split by lines.
fn pack_text(src: &str, range: Range<usize>, max: usize) -> Vec<Range<usize>> {
    let bytes = src.as_bytes();
    let mut paragraphs = Vec::new();
    let mut start = range.start;
    let mut pos = range.start;
    let mut prev_blank = false;
    for line in src[range.clone()].split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if prev_blank && !blank && pos > start {
            paragraphs.push(start..pos);
            start = pos;
        }
        prev_blank = blank;
        pos += line.len();
    }
    if start < range.end {
        paragraphs.push(start..range.end);
    }

    let mut out: Vec<Range<usize>> = Vec::new();
    let mut cur: Option<Range<usize>> = None;
    let mut cur_size = 0;
    for p in paragraphs {
        let size = nonws(&bytes[p.clone()]);
        if size > max {
            out.extend(cur.take());
            cur_size = 0;
            out.extend(split_lines(bytes, p, max));
        } else if cur_size + size > max {
            out.extend(cur.replace(p));
            cur_size = size;
        } else {
            cur = Some(cur.map_or(p.clone(), |c| c.start..p.end));
            cur_size += size;
        }
    }
    out.extend(cur);
    out
}

/// Splits `range` at line ends into pieces of at most `max` non-whitespace characters.
/// A single line longer than `max` is cut at character boundaries.
fn split_lines(src: &[u8], range: Range<usize>, max: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = range.start;
    let mut size = 0;
    let mut i = range.start;
    while i < range.end {
        let line_end = src[i..range.end]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(range.end, |p| i + p + 1);
        let line_size = nonws(&src[i..line_end]);
        if size + line_size > max && i > start {
            out.push(start..i);
            start = i;
            size = 0;
        }
        if line_size > max {
            let mut cut = i;
            let mut n = 0;
            for (off, &b) in src[i..line_end].iter().enumerate() {
                let at = i + off;
                // Only cut at UTF-8 character starts.
                if n >= max && (b & 0xC0) != 0x80 {
                    out.push(cut..at);
                    cut = at;
                    n = 0;
                }
                if !b.is_ascii_whitespace() && (b & 0xC0) != 0x80 {
                    n += 1;
                }
            }
            out.push(cut..line_end);
            start = line_end;
            size = 0;
        } else {
            size += line_size;
        }
        i = line_end;
    }
    if start < range.end {
        out.push(start..range.end);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Shared helpers

fn nonws(s: &[u8]) -> usize {
    s.iter()
        .filter(|b| !b.is_ascii_whitespace() && (**b & 0xC0) != 0x80)
        .count()
}

/// The span from the first to the last non-whitespace byte, or `None` if blank.
fn significant(src: &[u8], r: Range<usize>) -> Option<Range<usize>> {
    let s = &src[r.clone()];
    let first = s.iter().position(|b| !b.is_ascii_whitespace())?;
    let last = s.iter().rposition(|b| !b.is_ascii_whitespace())?;
    Some(r.start + first..r.start + last + 1)
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(src: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            src.bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { starts }
    }

    /// 1-based line of `byte`.
    fn line(&self, byte: usize) -> usize {
        self.starts.partition_point(|&s| s <= byte)
    }

    fn line_start(&self, line: usize) -> usize {
        self.starts[line - 1]
    }
}

#[allow(clippy::too_many_arguments)]
fn build(
    path: &str,
    lang: Lang,
    src: &str,
    lines: &LineIndex,
    r: Range<usize>,
    scope: Vec<String>,
    defs: &[&Tag],
    refs: &[&Tag],
) -> Option<Chunk> {
    let r_start = r.start;
    let sig = significant(src.as_bytes(), r)?;
    let start_line = lines.line(sig.start);
    let end_line = lines.line(sig.end - 1);
    let start_byte = lines.line_start(start_line).max(r_start);
    let end_byte = sig.end;
    let content = src[start_byte..end_byte].to_string();

    let in_span = |t: &&&Tag| t.name_byte >= start_byte && t.name_byte < end_byte;
    let mut symbols: Vec<String> = defs.iter().filter(in_span).map(|t| t.name.clone()).collect();
    dedup_keep_order(&mut symbols);
    let refs: Vec<&&Tag> = refs
        .iter()
        .filter(in_span)
        .filter(|t| t.kind != TagKind::Ref("implementation".into()))
        .collect();
    let call_tags: Vec<&&&Tag> = refs
        .iter()
        .filter(|t| t.kind == TagKind::Ref("call".into()))
        .collect();
    let mut calls: Vec<String> = call_tags.iter().map(|t| t.name.clone()).collect();
    dedup_keep_order(&mut calls);
    calls.truncate(64);
    let mut method_calls: Vec<String> = call_tags
        .iter()
        .filter(|t| t.member)
        .map(|t| t.name.clone())
        .filter(|n| calls.contains(n))
        .collect();
    dedup_keep_order(&mut method_calls);
    let mut refs: Vec<String> = refs.iter().map(|t| t.name.clone()).collect();
    dedup_keep_order(&mut refs);
    refs.truncate(64);

    let mut idh = blake3::Hasher::new();
    idh.update(path.as_bytes());
    idh.update(&[0]);
    idh.update(start_byte.to_string().as_bytes());
    idh.update(&[0]);
    idh.update(end_byte.to_string().as_bytes());
    idh.update(&[0]);
    idh.update(content.as_bytes());
    let id = idh.finalize().to_hex()[..32].to_string();
    let content_hash = blake3::hash(content.as_bytes()).to_hex()[..32].to_string();

    Some(Chunk {
        id,
        content_hash,
        path: path.to_string(),
        lang,
        start_byte,
        end_byte,
        start_line,
        end_line,
        scope,
        symbols,
        refs,
        calls,
        method_calls,
        content,
    })
}

fn dedup_keep_order(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: ChunkOptions = ChunkOptions {
        max_chars: 120,
        min_chars: 30,
    };

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

    #[test]
    fn chunks_tile_the_file_and_respect_the_budget() {
        let src = go_src();
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

    #[test]
    fn doc_comments_travel_with_their_function_and_symbols_are_tagged() {
        let src = go_src();
        let chunks = chunk_file("demo.go", Lang::Go, &src, ChunkOptions::default());
        let locate = chunks
            .iter()
            .find(|c| c.symbols.contains(&"Locate".to_string()))
            .unwrap();
        assert!(locate.content.contains("// Locate finds the binary."));
        assert!(chunks.iter().any(|c| c.refs.contains(&"Sprint".to_string())));
    }

    #[test]
    fn large_functions_are_split_inside_and_keep_their_scope() {
        let src = go_src();
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

        let ts =
            "export interface Opts { k: number }\nexport function search(o: Opts): number { return o.k }\n";
        let chunks = chunk_file("a.ts", Lang::TypeScript, ts, ChunkOptions::default());
        let syms: Vec<_> = chunks.iter().flat_map(|c| c.symbols.clone()).collect();
        assert!(syms.contains(&"search".into()), "{syms:?}");
    }

    #[test]
    fn calls_and_imports_are_extracted_per_language() {
        let cases: [(&str, Lang, &str, &[&str], &str); 7] = [
            (
                "a.go",
                Lang::Go,
                "package a\n\nimport (\n\t\"fmt\"\n\tx \"example.com/m/pkg/x\"\n)\n\nfunc F() { fmt.Println(x.Run()) }\n",
                &["fmt", "example.com/m/pkg/x"],
                "Run",
            ),
            (
                "src/lib.rs",
                Lang::Rust,
                "mod engine;\nuse crate::ledger::{Ledger, FileEntry};\n\nfn f() { open(); }\n",
                &["mod:engine", "crate::ledger::{Ledger, FileEntry}"],
                "open",
            ),
            (
                "a.py",
                Lang::Python,
                "import os.path\nfrom .util import helper\n\ndef f():\n    return helper(1)\n",
                &["os.path", ".util"],
                "helper",
            ),
            (
                "a.ts",
                Lang::TypeScript,
                "import { b } from './b';\nconst c = require('./c');\nexport function f() { return b(); }\n",
                &["./b", "./c"],
                "b",
            ),
            (
                "A.java",
                Lang::Java,
                "import com.acme.util.Strings;\nclass A { void f() { Strings.trim(\"x\"); } }\n",
                &["com.acme.util.Strings"],
                "trim",
            ),
            (
                "a.c",
                Lang::C,
                "#include \"util.h\"\n#include <stdio.h>\nint f(void) { return helper(1); }\n",
                &["util.h", "stdio.h"],
                "helper",
            ),
            (
                "a.cpp",
                Lang::Cpp,
                "#include \"util.hpp\"\nint f() { return ns::helper(1); }\n",
                &["util.hpp"],
                "helper",
            ),
        ];
        for (path, lang, src, want, call) in cases {
            let (chunks, imports) = analyze_file(path, lang, src, ChunkOptions::default());
            assert_eq!(imports, want, "{path}");
            assert!(
                chunks.iter().any(|c| c.calls.iter().any(|n| n == call)),
                "{path}: no call to {call} in {:?}",
                chunks.iter().map(|c| &c.calls).collect::<Vec<_>>()
            );
        }
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
}
