//! Hashing of syntax subtrees, the base of clone detection and of the rstest check.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use tree_sitter::Node;

/// What is wildcarded before a subtree is hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalize {
    /// Identifiers and literals: two blocks that differ only in names and constants are equal.
    Structure,
    /// Literals only: two blocks that differ only in constants are equal.
    Literals,
}

/// A subtree big enough to be a clone candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub hash: u64,
    /// Leaves of the syntax tree under it.
    pub tokens: usize,
    pub bytes: Range<usize>,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
}

/// Which subtrees to record: at least this many lines and tokens, outside the skipped byte ranges.
#[derive(Debug, Clone, Copy)]
pub struct Limits<'a> {
    pub min_lines: usize,
    pub min_tokens: usize,
    pub skip: &'a [Range<usize>],
}

/// Subtrees deeper than this are hashed by their kind alone, so a pathological file cannot overflow the stack.
const MAX_DEPTH: usize = 400;

fn is_identifier(kind: &str) -> bool {
    kind == "identifier" || kind.ends_with("_identifier")
}

fn is_literal(kind: &str) -> bool {
    kind.contains("literal")
        || matches!(
            kind,
            "string" | "template_string" | "number" | "integer" | "float" | "interpreted_string_literal"
        )
}

fn hash_of(parts: &[&dyn Fn(&mut DefaultHasher)]) -> u64 {
    let mut h = DefaultHasher::new();
    for part in parts {
        part(&mut h);
    }
    h.finish()
}

/// Hash and token count of `node`, recording every subtree that meets the limits into `out`.
pub fn walk(
    node: Node,
    src: &[u8],
    mode: Normalize,
    lim: &Limits,
    out: &mut Vec<Unit>,
    depth: usize,
) -> (u64, usize) {
    let kind = node.kind();
    let literal = is_literal(kind);
    if literal || node.child_count() == 0 || depth > MAX_DEPTH {
        let hash = if literal {
            hash_of(&[&|h| "$lit".hash(h)])
        } else if is_identifier(kind) && mode == Normalize::Structure {
            hash_of(&[&|h| "$id".hash(h)])
        } else if is_identifier(kind) {
            let text = &src[node.byte_range()];
            hash_of(&[&|h| "id".hash(h), &|h| text.hash(h)])
        } else {
            hash_of(&[&|h| kind.hash(h)])
        };
        return (hash, 1);
    }
    let mut h = DefaultHasher::new();
    kind.hash(&mut h);
    let mut tokens = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let range = child.byte_range();
        if child.is_extra()
            || lim
                .skip
                .iter()
                .any(|s| s.start <= range.start && range.end <= s.end)
        {
            continue;
        }
        let (ch, ct) = walk(child, src, mode, lim, out, depth + 1);
        ch.hash(&mut h);
        tokens += ct;
    }
    let hash = h.finish();
    let (start, end) = (node.start_position().row + 1, node.end_position().row + 1);
    if end + 1 - start >= lim.min_lines && tokens >= lim.min_tokens && node.parent().is_some() {
        out.push(Unit {
            hash,
            tokens,
            bytes: node.byte_range(),
            start_line: start,
            end_line: end,
        });
    }
    (hash, tokens)
}
