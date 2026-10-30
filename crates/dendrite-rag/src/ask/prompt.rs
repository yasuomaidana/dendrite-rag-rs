//! Prompts and answer post-processing for `ask`, carried over from Dendrite's Go
//! implementation (pkg/rag/ask.go) so answers do not change with the move.

use loom_rag::engine::Hit;

pub(crate) const SYSTEM_PROMPT: &str = "You are Dendrite, an AI development and infrastructure assistant for this project. \
Answer the user's question accurately, concisely, and helpfully based on the retrieved project context. \
Include concrete CLI commands or instructions if requested.";

pub(crate) const PAID_SYSTEM_PROMPT: &str = "You are Dendrite AI Assistant. Provide accurate, concise answers based on the retrieved project context.";

/// Marker of a canned engine reply that does not answer the question.
pub(crate) const CANNED_REPLY: &str = "processed your request safely with zero cloud tokens consumed";

const RULE: &str = "====================================================================\n";
const SEPARATOR: &str = "\n--------------------------------------------------------------------\n";

fn context_block(hits: &[Hit], empty_note: bool) -> String {
    let mut s = String::from("Retrieved Project Context:\n");
    s.push_str(RULE);
    if hits.is_empty() && empty_note {
        s.push_str("(No specific documentation or code matches retrieved from local index)\n");
    }
    for (i, h) in hits.iter().enumerate() {
        s.push_str(&format!(
            "[{}] Source: {} (lines {}-{})\n",
            i + 1,
            h.path,
            h.start_line,
            h.end_line
        ));
        if !h.scope.is_empty() {
            s.push_str(&format!("    Heading: {}\n", h.scope.join(" › ")));
        }
        s.push_str(&h.content);
        s.push_str(SEPARATOR);
    }
    s.push_str(RULE);
    s
}

/// The user prompt: numbered sources with `path (lines a-b)`, then the question.
pub(crate) fn rag_prompt(question: &str, hits: &[Hit]) -> String {
    format!(
        "{}\nUser Question: {question}\n\nAnswer:",
        context_block(hits, true)
    )
}

/// The prompt asking a stronger model to verify and refine a local draft.
pub(crate) fn refinement_prompt(question: &str, hits: &[Hit], draft: &str) -> String {
    let mut s = String::from(
        "You are Dendrite AI Refinement Assistant. A local zero-cost LLM generated an initial draft answer based on project context, but it requires professional refinement, verification, and precision.\n\n",
    );
    s.push_str(&context_block(hits, false));
    s.push_str(&format!("\nUser Question: {question}\n\n"));
    s.push_str(&format!("Initial Local Draft:\n{draft}\n\n"));
    s.push_str("Instructions:\n");
    s.push_str("1. Verify the accuracy of the local draft against the retrieved project context.\n");
    s.push_str("2. Eliminate any hallucinations, canned placeholders, or inaccuracies.\n");
    s.push_str("3. Ensure exact, copy-pasteable CLI commands or code snippets are provided if requested.\n");
    s.push_str("4. Keep the refined answer concise, high-signal, and production-ready.\n\n");
    s.push_str("Refined Answer:");
    s
}

/// Up to five distinct `dendrite …` commands quoted in the retrieved spans.
fn commands(hits: &[Hit]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in hits.iter().flat_map(|h| h.content.lines()) {
        let mut t = line.trim();
        for p in ["$ ", "```bash", "```sh", "```"] {
            t = t.strip_prefix(p).unwrap_or(t);
        }
        let t = t.trim();
        if t.starts_with("dendrite ") && !t.contains('<') && !t.starts_with("dendrite ask") {
            let cmd = t.trim_matches(|c| matches!(c, '`' | '\'' | '"')).to_string();
            if !out.contains(&cmd) && out.len() < 5 {
                out.push(cmd);
            }
        }
    }
    out
}

/// Replaces a canned or empty answer with the commands the context documents, and appends
/// them when the question asks for commands and the answer names none.
pub(crate) fn enhance(question: &str, answer: &str, hits: &[Hit]) -> String {
    let q = question.to_lowercase();
    let wants_command = ["command", "how to", "check", "show", "run"]
        .iter()
        .any(|w| q.contains(w));
    let cmds = commands(hits);
    let trimmed = answer.trim();
    let canned = trimmed.contains(CANNED_REPLY) || trimmed.chars().count() < 20;
    if canned {
        if q.contains("available llm") || q.contains("check available") || q.contains("list model") {
            return "To check available LLMs and inference engines in Dendrite, execute the following commands:\n\n\
1. List all registered engines, models, and fallback cascade order:\n   $ dendrite spike list\n   (alias: dendrite inference list)\n\n\
2. Inspect configured zero-cost and self-hosted models/prefixes:\n   $ dendrite spike zero-cost list\n\n\
3. Probe local Ollama service health, latency, and loaded models:\n   $ dendrite spike probe --endpoint http://127.0.0.1:11434\n\n\
4. Probe real-time reachability and latency across all engines:\n   $ dendrite spike probe"
                .to_string();
        }
        if !cmds.is_empty() {
            let mut s =
                String::from("Based on the retrieved project documentation, run the following commands:\n\n");
            for (i, c) in cmds.iter().enumerate() {
                s.push_str(&format!("{}. `$ {c}`\n", i + 1));
            }
            return s;
        }
    }
    if wants_command && !cmds.is_empty() && !answer.contains("dendrite ") {
        let mut s = String::from(trimmed);
        s.push_str("\n\n[+] Key Commands from Project Documentation:\n");
        for c in &cmds {
            s.push_str(&format!("  $ {c}\n"));
        }
        return s;
    }
    answer.to_string()
}
