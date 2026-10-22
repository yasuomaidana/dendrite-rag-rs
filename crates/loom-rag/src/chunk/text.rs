//! Markdown sections by heading (outside code fences) and line-packed plain text.

use super::*;

// ---------------------------------------------------------------------------------------------
// Markdown and text

/// Splits Markdown by headings outside code fences; returns spans with their heading path.
pub(super) fn markdown_sections(src: &str, opts: ChunkOptions) -> Vec<(Range<usize>, Vec<String>)> {
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
pub(super) fn pack_text(src: &str, range: Range<usize>, max: usize) -> Vec<Range<usize>> {
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
pub(super) fn split_lines(src: &[u8], range: Range<usize>, max: usize) -> Vec<Range<usize>> {
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

pub(super) fn nonws(s: &[u8]) -> usize {
    s.iter()
        .filter(|b| !b.is_ascii_whitespace() && (**b & 0xC0) != 0x80)
        .count()
}

/// The span from the first to the last non-whitespace byte, or `None` if blank.
pub(super) fn significant(src: &[u8], r: Range<usize>) -> Option<Range<usize>> {
    let s = &src[r.clone()];
    let first = s.iter().position(|b| !b.is_ascii_whitespace())?;
    let last = s.iter().rposition(|b| !b.is_ascii_whitespace())?;
    Some(r.start + first..r.start + last + 1)
}
