//! Formatting-stable hashes of definitions (`symbols.hashes`, DRAG-124).
//!
//! A definition's hash covers its tokens only: whitespace, layout and comments do not change it, a
//! renamed identifier, an edited body or a changed signature do. Other tools use the hash as a key for
//! "the same symbol, unchanged" (leases on symbols, drift checks), so it must survive a reformat.

use super::*;

/// A definition and the hash of its token stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolHash {
    pub symbol: String,
    /// The tag query's kind: `function`, `method`, `class`, `type`, ...
    pub kind: String,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// `blake3` of the definition's tokens, first 32 hex chars.
    pub hash: String,
}

/// Token spans of `node` in source order, comments left out.
fn tokens(node: Node, out: &mut Vec<Range<usize>>) {
    if node.kind().contains("comment") {
        return;
    }
    if node.child_count() == 0 {
        if node.end_byte() > node.start_byte() {
            out.push(node.byte_range());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        tokens(child, out);
    }
}

/// The hash of the tokens inside `span`: `all` is sorted by start, so the slice is found by bisection.
fn span_hash(all: &[Range<usize>], span: &Range<usize>, src: &str) -> String {
    let from = all.partition_point(|t| t.start < span.start);
    let mut hasher = blake3::Hasher::new();
    for token in all[from..].iter().take_while(|t| t.end <= span.end) {
        hasher.update(src[token.clone()].as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex()[..32].to_string()
}

/// Every definition of a source file with its hash, in source order. Languages without a tag query
/// (Markdown, plain text) and sources that do not parse give nothing. A nested definition is hashed
/// on its own, and its tokens also count in the definition that contains it.
pub fn symbol_hashes(lang: Lang, src: &str) -> Vec<SymbolHash> {
    let mut parser = Parser::new();
    let Some(tree) = lang
        .grammar()
        .filter(|g| parser.set_language(g).is_ok())
        .and_then(|_| parser.parse(src, None))
    else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut all = Vec::new();
    tokens(root, &mut all);
    let lines = LineIndex::new(src);
    let mut out: Vec<(usize, SymbolHash)> = code::tags(lang, root, src.as_bytes())
        .into_iter()
        .filter_map(|t| match t.kind {
            TagKind::Def(kind) => Some((
                t.range.start,
                SymbolHash {
                    symbol: t.name,
                    kind,
                    start_line: lines.line(t.range.start),
                    end_line: lines.line(t.range.end.saturating_sub(1).max(t.range.start)),
                    hash: span_hash(&all, &t.range, src),
                },
            )),
            TagKind::Ref(_) => None,
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.symbol.cmp(&b.1.symbol)));
    out.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    out.into_iter().map(|(_, h)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn hash_of(lang: Lang, src: &str, symbol: &str) -> String {
        symbol_hashes(lang, src)
            .into_iter()
            .find(|h| h.symbol == symbol)
            .unwrap_or_else(|| panic!("no definition {symbol} in:\n{src}"))
            .hash
    }

    const RUST: &str = "fn add(a: i32, b: i32) -> i32 { a + b }\n\nfn twice(x: i32) -> i32 { add(x, x) }\n";
    const GO: &str = "package p\n\nfunc Add(a, b int) int { return a + b }\n\nfunc Twice(x int) int { return Add(x, x) }\n";
    const PY: &str = "def add(a, b):\n    return a + b\n\n\ndef twice(x):\n    return add(x, x)\n";
    const TS: &str = "function add(a: number, b: number): number { return a + b }\n\nfunction twice(x: number): number { return add(x, x) }\n";

    #[rstest]
    #[case::rust(
        Lang::Rust,
        RUST,
        "add",
        "fn add(a: i32,\n    b: i32) -> i32 {\n    // sum\n    a + b\n}\n\n\nfn twice(x: i32) -> i32 { add(x, x) }\n"
    )]
    #[case::go(
        Lang::Go,
        GO,
        "Add",
        "package p\n\n// Add sums.\nfunc Add(a, b int) int {\n\t// sum\n\treturn a + b\n}\n\nfunc Twice(x int) int { return Add(x, x) }\n"
    )]
    #[case::python(
        Lang::Python,
        PY,
        "add",
        "def add(a, b):\n    # sum\n    return a + b\n\n\n\ndef twice(x):\n    return add(x, x)\n"
    )]
    #[case::typescript(
        Lang::TypeScript,
        TS,
        "add",
        "function add(\n  a: number,\n  b: number\n): number {\n  /* sum */\n  return a + b\n}\n\nfunction twice(x: number): number { return add(x, x) }\n"
    )]
    fn a_reformat_or_a_comment_leaves_the_hash_alone(
        #[case] lang: Lang,
        #[case] before: &str,
        #[case] symbol: &str,
        #[case] after: &str,
    ) {
        assert_eq!(hash_of(lang, before, symbol), hash_of(lang, after, symbol));
    }

    #[rstest]
    #[case::rust(
        Lang::Rust,
        RUST,
        "add",
        "fn add(a: i32, b: i32) -> i32 { a - b }\n\nfn twice(x: i32) -> i32 { add(x, x) }\n",
        "twice"
    )]
    #[case::go(
        Lang::Go,
        GO,
        "Add",
        "package p\n\nfunc Add(a, b int) int { return a - b }\n\nfunc Twice(x int) int { return Add(x, x) }\n",
        "Twice"
    )]
    #[case::python(
        Lang::Python,
        PY,
        "add",
        "def add(a, b):\n    return a - b\n\n\ndef twice(x):\n    return add(x, x)\n",
        "twice"
    )]
    #[case::typescript(
        Lang::TypeScript,
        TS,
        "add",
        "function add(a: number, b: number): number { return a - b }\n\nfunction twice(x: number): number { return add(x, x) }\n",
        "twice"
    )]
    fn an_edit_changes_that_symbol_and_no_other(
        #[case] lang: Lang,
        #[case] before: &str,
        #[case] edited: &str,
        #[case] after: &str,
        #[case] untouched: &str,
    ) {
        assert_ne!(hash_of(lang, before, edited), hash_of(lang, after, edited));
        assert_eq!(hash_of(lang, before, untouched), hash_of(lang, after, untouched));
    }

    #[test]
    fn a_signature_change_changes_the_hash() {
        let wider = RUST.replace("fn add(a: i32, b: i32)", "fn add(a: i64, b: i32)");
        assert_ne!(
            hash_of(Lang::Rust, RUST, "add"),
            hash_of(Lang::Rust, &wider, "add")
        );
    }

    #[test]
    fn definitions_carry_their_kind_and_lines() {
        let hashes = symbol_hashes(Lang::Rust, RUST);
        let names: Vec<_> = hashes
            .iter()
            .map(|h| (h.symbol.as_str(), h.start_line, h.end_line))
            .collect();
        assert_eq!(names, [("add", 1, 1), ("twice", 3, 3)]);
        assert!(hashes.iter().all(|h| h.kind == "function" && h.hash.len() == 32));
    }

    #[test]
    fn renaming_a_callee_changes_the_hash_of_its_caller() {
        let renamed = RUST.replace("add", "plus");
        assert_ne!(
            hash_of(Lang::Rust, RUST, "twice"),
            hash_of(Lang::Rust, &renamed, "twice")
        );
    }

    #[test]
    fn languages_without_tags_and_empty_sources_give_nothing() {
        assert!(symbol_hashes(Lang::Markdown, "# Title\n\ntext\n").is_empty());
        assert!(symbol_hashes(Lang::Text, "plain").is_empty());
        assert!(symbol_hashes(Lang::Rust, "").is_empty());
    }
}
