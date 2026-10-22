//! Golden query sets (`eval/golden-*.json`).

use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct File {
    queries: Vec<Query>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    pub q: String,
    /// Files that answer the query; any of them counts as a hit.
    pub expect: Vec<String>,
}

impl Query {
    /// Whether the expected files span both corpora (a mixed question).
    pub fn is_mixed(&self) -> bool {
        self.expect.iter().any(|p| is_doc(p)) && self.expect.iter().any(|p| !is_doc(p))
    }
}

/// A golden set: its name (the file stem) and queries.
pub struct Golden {
    pub name: String,
    pub queries: Vec<Query>,
}

pub fn load(path: &Path) -> anyhow::Result<Golden> {
    let file: File = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("golden")
        .trim_start_matches("golden-")
        .to_string();
    Ok(Golden {
        name,
        queries: file.queries,
    })
}

/// Documentation paths, by the rule of `loom_rag::corpus` (Markdown, txt, rst).
pub fn is_doc(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".mdx", ".txt", ".rst"]
        .iter()
        .any(|e| lower.ends_with(e))
}
