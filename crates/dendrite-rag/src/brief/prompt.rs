//! The prompt of `brief`, the check that its answer cites only what it was given, and the
//! deterministic brief returned when no model could write one.

use std::collections::HashSet;

use loom_rag::engine::Hit;
use loom_rag::retrieve::lookup::GrepMatch;

pub(crate) const SYSTEM_PROMPT: &str = "You prepare a reading brief for a coding agent that will open files itself. \
You are given a question, a map of spans retrieved from the project, and grep matches. \
Answer ONLY with these three sections, in this order, using ONLY paths and line numbers that appear in the input. \
Replace every <placeholder> with real content and never repeat the placeholder text:\n\
Files to read:\n- <path>:<start>-<end> - <what it holds, under 10 words>\n\
Key symbols:\n- <name> (<path>:<line>) - <what it is, under 10 words>\n\
Gaps:\n- <what the input does not show>\n\
At most 10 files, one line each. Do not answer the question itself and do not invent paths.";

/// Longest text of a span in the prompt: a map entry needs a declaration, not a body.
const SPAN_CHARS: usize = 400;

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The user prompt: numbered spans (path, lines, heading, outline), the grep matches, the question.
pub(crate) fn user_prompt(question: &str, hits: &[Hit], matches: &[GrepMatch]) -> String {
    let mut s = String::from("Map of retrieved spans:\n");
    if hits.is_empty() {
        s.push_str("(none)\n");
    }
    for (i, h) in hits.iter().enumerate() {
        s.push_str(&format!("[{}] {}:{}-{}", i + 1, h.path, h.start_line, h.end_line));
        if !h.scope.is_empty() {
            s.push_str(&format!("  ({})", h.scope.join(" › ")));
        }
        s.push('\n');
        s.push_str(clip(h.content.trim(), SPAN_CHARS).trim_end());
        s.push('\n');
    }
    s.push_str("\nGrep matches:\n");
    if matches.is_empty() {
        s.push_str("(none)\n");
    }
    for m in matches {
        s.push_str(&format!("{}:{}: {}\n", m.path, m.line, m.text));
    }
    s.push_str(&format!("\nQuestion: {question}\n\nBrief:"));
    s
}

/// Every path the model may cite.
pub(crate) fn known_paths<'a>(hits: &'a [Hit], matches: &'a [GrepMatch]) -> HashSet<&'a str> {
    hits.iter()
        .map(|h| h.path.as_str())
        .chain(matches.iter().map(|m| m.path.as_str()))
        .collect()
}

/// The placeholders of the system prompt; a line still holding one was copied, not written.
const PLACEHOLDERS: [&str; 5] = ["<path>", "<start>", "<end>", "<line>", "<name>"];

/// `brief` without the lines that cite a path the input did not contain: a model inventing a file
/// would send the agent after something that does not exist. Lines that still hold a placeholder
/// of the prompt are dropped too. Only tokens with a directory and an
/// extension count as paths; a bare `serde.json` or `v1.2` is prose.
pub(crate) fn keep_cited(brief: &str, known: &HashSet<&str>) -> String {
    let kept: Vec<&str> = brief
        .lines()
        .filter(|line| !PLACEHOLDERS.iter().any(|p| line.contains(p)) && cites_only(line, known))
        .collect();
    kept.join("\n").trim().to_string()
}

fn cites_only(line: &str, known: &HashSet<&str>) -> bool {
    line.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')))
        .map(|t| t.trim_matches('.'))
        .filter(|t| is_path(t))
        .all(|t| known.iter().any(|k| *k == t || k.ends_with(t) || t.ends_with(*k)))
}

fn is_path(token: &str) -> bool {
    !token.starts_with('/')
        && token
            .rsplit_once('/')
            .is_some_and(|(dir, file)| !dir.is_empty() && file.contains('.') && !file.ends_with('.'))
}

/// The brief when no model wrote one: the ranked spans, then the matches.
pub(crate) fn raw_brief(hits: &[Hit], matches: &[GrepMatch]) -> String {
    if hits.is_empty() && matches.is_empty() {
        return "Nothing in the project matched the question.".to_string();
    }
    let mut s = String::from("Files (ranked):\n");
    for h in hits {
        s.push_str(&format!("- {}:{}-{}", h.path, h.start_line, h.end_line));
        if !h.scope.is_empty() {
            s.push_str(&format!(" ({})", h.scope.join(" › ")));
        }
        s.push('\n');
    }
    s.push_str("Matches:\n");
    for m in matches {
        s.push_str(&format!("- {}:{}: {}\n", m.path, m.line, m.text));
    }
    s.trim_end().to_string()
}
