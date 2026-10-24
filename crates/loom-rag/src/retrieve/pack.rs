//! Packing retrieved spans for a prompt: token estimates, de-duplication of identical text,
//! outlines (signatures) for spans worth a pointer but not their body, greedy packing into a
//! budget and merging of touching spans.

use std::collections::HashMap;

use super::{ContextPack, Timings};
use crate::engine::Hit;

/// Lines an outline keeps: a span's doc comment or heading and its declaration.
const OUTLINE_LINES: usize = 2;
/// Characters kept of each outline line.
const OUTLINE_LINE_CHARS: usize = 160;

/// Estimated tokens: ~4 characters per token with a 1.15 safety factor, plus a few for
/// the citation line.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() as f32 / 4.0 * 1.15).ceil() as usize + 8
}

/// The outline of a span's text: its first meaningful lines (a doc comment or heading and
/// the declaration or first sentence), each cut to a line's worth of characters.
pub fn signature(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !matches!(*l, "---" | "```" | "{" | "}") && !is_preamble(l))
        .take(OUTLINE_LINES)
        .map(|l| match l.char_indices().nth(OUTLINE_LINE_CHARS) {
            Some((i, _)) => format!("{}…", &l[..i]),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Package, import and include lines, which say nothing about what a span does.
fn is_preamble(line: &str) -> bool {
    ["package ", "import ", "use ", "#include", "from ", "mod "]
        .iter()
        .any(|p| line.starts_with(p))
        || line == "import ("
}

/// Replaces a hit's text with its outline.
pub(super) fn outline(mut h: Hit) -> Hit {
    h.content = signature(&h.content);
    h.outline = true;
    h
}

/// Drops hits whose text a better hit already holds, recording their paths on it.
pub(super) fn dedup(hits: Vec<Hit>) -> Vec<Hit> {
    let mut first: HashMap<blake3::Hash, usize> = HashMap::new();
    let mut out: Vec<Hit> = Vec::with_capacity(hits.len());
    for h in hits {
        let key = blake3::hash(h.content.trim().as_bytes());
        match first.get(&key) {
            Some(&i) if out[i].path != h.path && !out[i].duplicates.contains(&h.path) => {
                out[i].duplicates.push(h.path)
            }
            Some(_) => {}
            None => {
                first.insert(key, out.len());
                out.push(h);
            }
        }
    }
    out
}

/// Packs spans (best first) greedily into `token_budget`, skipping spans already covered
/// by a better one, then merges spans that touch within a file. A span whose text no longer
/// fits is outlined instead when its outline does (DRAG-51), so the pack still points to it.
pub(super) fn pack(spans: Vec<Hit>, token_budget: usize) -> ContextPack {
    let mut picked: Vec<Hit> = Vec::new();
    let mut tokens = 0;
    for s in spans {
        let covered = picked
            .iter()
            .any(|p| p.path == s.path && p.start_line <= s.start_line && p.end_line >= s.end_line);
        if covered {
            continue;
        }
        let cost = estimate_tokens(&s.content);
        let s = if tokens + cost <= token_budget {
            s
        } else if s.outline {
            continue;
        } else {
            outline(s)
        };
        let cost = estimate_tokens(&s.content);
        if tokens + cost > token_budget {
            continue;
        }
        tokens += cost;
        picked.push(s);
    }
    ContextPack {
        spans: merge_adjacent(picked),
        tokens,
        abstained: false,
        timings: Timings::default(),
    }
}

pub(super) fn merge_adjacent(mut spans: Vec<Hit>) -> Vec<Hit> {
    spans.sort_by(|a, b| a.path.cmp(&b.path).then(a.start_line.cmp(&b.start_line)));
    let mut out: Vec<Hit> = Vec::new();
    for s in spans {
        // An outline stands for a chunk the pack does not include, so it never joins a span.
        if let Some(last) = out.last_mut()
            && last.path == s.path
            && !last.outline
            && !s.outline
            && s.start_line <= last.end_line + 1
        {
            if s.end_line > last.end_line {
                last.content.push('\n');
                last.content.push_str(&s.content);
                last.end_line = s.end_line;
            }
            last.score = last.score.max(s.score);
            continue;
        }
        out.push(s);
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}
