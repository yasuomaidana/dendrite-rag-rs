//! Syntax-aware chunking of code: tree-sitter tags, cAST splitting of large nodes, merging
//! of small spans, and import extraction.

use super::*;

pub(super) fn relang(mut c: Chunk, lang: Lang) -> Chunk {
    c.lang = lang;
    c
}

// ---------------------------------------------------------------------------------------------
// Code

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TagKind {
    Def(String),
    Ref(String),
}

#[derive(Debug, Clone)]
pub(super) struct Tag {
    pub(super) kind: TagKind,
    pub(super) name: String,
    pub(super) name_byte: usize,
    pub(super) range: Range<usize>,
    /// The name is the member of a receiver or qualifier expression (`a.name`).
    pub(super) member: bool,
}

pub(super) fn chunk_code(
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
