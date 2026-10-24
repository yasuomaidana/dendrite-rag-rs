//! cAST chunking over tree-sitter syntax trees, plus Markdown and text splitters
//! (`docs/specs/chunking.md`).

use std::ops::Range;

use serde::{Deserialize, Serialize};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, QueryCursor};

use crate::lang::Lang;

mod code;
use code::{Tag, TagKind, chunk_code, relang};
mod text;
use text::{markdown_sections, nonws, pack_text, significant, split_lines};

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
    /// A short text situating the chunk in its file (contextual retrieval, DRAG-48):
    /// indexed with the chunk, never returned in results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
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

    /// Header, context and content: the text BM25 and the embedder see.
    pub fn indexed_text(&self) -> String {
        match &self.context {
            Some(ctx) => format!("{}// context: {ctx}\n{}", self.header(), self.content),
            None => format!("{}{}", self.header(), self.content),
        }
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
        context: None,
        content,
    })
}

fn dedup_keep_order(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

#[cfg(test)]
mod tests;
