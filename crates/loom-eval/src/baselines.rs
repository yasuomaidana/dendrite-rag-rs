//! Baselines over the same files the engine indexes:
//! - `grep-agent`: what a grep-driven coding agent reads. Files are ranked by how many
//!   distinct query words they contain, then by occurrences, and read whole.
//! - `bm25-file`: BM25 over whole files (the code-aware tokenizer, no chunking).
//! - `naive-chunks`: BM25 over fixed 40-line windows (no syntax-aware chunking, no
//!   symbols, no fusion).

use std::collections::HashMap;
use std::path::Path;

use loom_rag::chunk::Chunk;
use loom_rag::lexical::LexicalIndex;
use loom_rag::retrieve::estimate_tokens;
use loom_rag::walk::{WalkOptions, read_text, walk};

use crate::metrics::Unit;

const TOP: usize = 50;
const WINDOW_LINES: usize = 40;

/// Words a grep for a natural-language question would not search for.
const STOPWORDS: &str = "\
    the and for how does what which where when with from into that this are its their there about work \
    works use using get can should who why you your our has have been was were will per all any not but \
    only";

struct File {
    path: String,
    text: String,
    lower: String,
    tokens: usize,
    /// Other files with the same text: credited like the engine's `duplicates`.
    copies: Vec<String>,
}

pub struct Baselines {
    files: Vec<File>,
    whole: LexicalIndex,
    windows: LexicalIndex,
    /// Chunk id → (path, tokens, copies) for both lexical indexes.
    units: HashMap<String, (String, usize, Vec<String>)>,
    _dir: tempfile::TempDir,
}

impl Baselines {
    /// Reads the files under `root` the engine indexes (`walk`: the project's own walk
    /// settings) and builds the two BM25 indexes.
    pub fn build(root: &Path, walk_options: &WalkOptions) -> anyhow::Result<Self> {
        let mut files = Vec::new();
        for f in walk(root, walk_options)? {
            if let Some(text) = read_text(&f.abs) {
                files.push(File {
                    path: f.path,
                    lower: text.to_lowercase(),
                    tokens: estimate_tokens(&text),
                    text,
                    copies: Vec::new(),
                });
            }
        }
        let mut by_text: HashMap<String, Vec<String>> = HashMap::new();
        for f in &files {
            by_text
                .entry(f.text.trim().to_string())
                .or_default()
                .push(f.path.clone());
        }
        for f in &mut files {
            f.copies = by_text[f.text.trim()]
                .iter()
                .filter(|p| **p != f.path)
                .cloned()
                .collect();
        }
        let dir = tempfile::tempdir()?;
        let whole = LexicalIndex::open(&dir.path().join("whole"))?;
        let windows = LexicalIndex::open(&dir.path().join("windows"))?;
        let mut units = HashMap::new();
        {
            let mut ww = whole.writer()?;
            let mut nw = windows.writer()?;
            for f in &files {
                let id = format!("file:{}", f.path);
                ww.add(&pseudo_chunk(
                    &id,
                    &f.path,
                    &f.text,
                    1,
                    f.text.lines().count().max(1),
                ))?;
                units.insert(id, (f.path.clone(), f.tokens, f.copies.clone()));
                let lines: Vec<&str> = f.text.lines().collect();
                for (i, window) in lines.chunks(WINDOW_LINES).enumerate() {
                    let id = format!("win:{}:{i}", f.path);
                    let text = window.join("\n");
                    let start = i * WINDOW_LINES + 1;
                    nw.add(&pseudo_chunk(
                        &id,
                        &f.path,
                        &text,
                        start,
                        start + window.len() - 1,
                    ))?;
                    // A window of a copied file is a copy of the same window.
                    units.insert(id, (f.path.clone(), estimate_tokens(&text), f.copies.clone()));
                }
            }
            ww.commit()?;
            nw.commit()?;
        }
        Ok(Self {
            files,
            whole,
            windows,
            units,
            _dir: dir,
        })
    }

    pub fn grep(&self, query: &str) -> Vec<Unit> {
        let mut words: Vec<String> = query
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(str::to_lowercase)
            .filter(|w| w.len() >= 3 && !STOPWORDS.split_whitespace().any(|s| s == w))
            .collect();
        words.sort();
        words.dedup();
        let mut ranked: Vec<(usize, usize, &File)> = self
            .files
            .iter()
            .filter_map(|f| {
                let counts: Vec<usize> = words
                    .iter()
                    .map(|w| f.lower.matches(w.as_str()).count())
                    .collect();
                let distinct = counts.iter().filter(|&&c| c > 0).count();
                (distinct > 0).then(|| (distinct, counts.iter().sum(), f))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.path.cmp(&b.2.path)));
        ranked
            .into_iter()
            .take(TOP)
            .map(|(_, _, f)| Unit {
                path: f.path.clone(),
                also: f.copies.clone(),
                tokens: f.tokens,
            })
            .collect()
    }

    pub fn bm25_files(&self, query: &str) -> anyhow::Result<Vec<Unit>> {
        self.lexical(&self.whole, query)
    }

    pub fn naive_chunks(&self, query: &str) -> anyhow::Result<Vec<Unit>> {
        self.lexical(&self.windows, query)
    }

    fn lexical(&self, index: &LexicalIndex, query: &str) -> anyhow::Result<Vec<Unit>> {
        Ok(index
            .search(query, TOP, &Default::default())?
            .into_iter()
            .filter_map(|(id, _)| self.units.get(&id))
            .map(|(path, tokens, copies)| Unit {
                path: path.clone(),
                also: copies.clone(),
                tokens: *tokens,
            })
            .collect())
    }
}

/// A chunk carrying only what the lexical index reads.
fn pseudo_chunk(id: &str, path: &str, text: &str, start_line: usize, end_line: usize) -> Chunk {
    let lang = loom_rag::lang::Lang::from_path(Path::new(path)).unwrap_or(loom_rag::lang::Lang::Text);
    Chunk {
        id: id.to_string(),
        content_hash: String::new(),
        path: path.to_string(),
        lang,
        start_byte: 0,
        end_byte: text.len(),
        start_line,
        end_line,
        scope: Vec::new(),
        symbols: Vec::new(),
        refs: Vec::new(),
        calls: Vec::new(),
        method_calls: Vec::new(),
        context: None,
        content: text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_ranks_by_distinct_words_and_reads_whole_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("gateway.go"),
            "// token budget gateway\nfunc Budget() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("other.go"),
            "// token token token\nfunc Other() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "# Unrelated\n").unwrap();
        let b = Baselines::build(dir.path(), &WalkOptions::default()).unwrap();
        let hits = b.grep("how does the token budget work?");
        let paths: Vec<&str> = hits.iter().map(|u| u.path.as_str()).collect();
        assert_eq!(
            paths,
            ["gateway.go", "other.go"],
            "two distinct words beat three repeats of one"
        );
        assert!(hits[0].tokens > 8, "a file is read whole");
        assert_eq!(b.bm25_files("Budget").unwrap()[0].path, "gateway.go");
        assert_eq!(b.naive_chunks("Budget").unwrap()[0].path, "gateway.go");
    }
}
