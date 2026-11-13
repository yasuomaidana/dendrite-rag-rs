//! Exact lookups: symbol definitions and references, and regex search over files.

use std::sync::Mutex;

use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Error, Hit, Result};
use crate::history::{HistoryHit, HistoryIndex, HistoryReport};
use crate::walk::{read_text, walk};

/// One regex match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepMatch {
    pub path: String,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

/// Longest line text returned for a match.
const MAX_LINE: usize = 400;

impl Engine {
    /// Chunks that define `name`.
    pub fn definitions(&self, name: &str) -> Result<Vec<Hit>> {
        let idx = self.symbol_index()?;
        self.ranked_ids(&idx.definitions(name))
    }

    /// Chunks that reference `name`.
    pub fn references(&self, name: &str) -> Result<Vec<Hit>> {
        let idx = self.symbol_index()?;
        self.ranked_ids(&idx.references(name))
    }

    fn ranked_ids(&self, ids: &[String]) -> Result<Vec<Hit>> {
        let ranked: Vec<(String, f32)> = ids.iter().map(|id| (id.clone(), 1.0)).collect();
        let mut hits = self.hits(&ranked)?;
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.start_line.cmp(&b.start_line)));
        Ok(hits)
    }

    /// Brings the git-history index up to date (incrementally from the last indexed commit).
    pub fn index_history(&self, max_commits: usize) -> Result<HistoryReport> {
        let history = HistoryIndex::open(&self.dir.join("history")).map_err(|e| Error::Other(e.into()))?;
        let since = self.ledger.meta("history_head")?;
        let report = history
            .update(&self.root, since.as_deref(), max_commits)
            .map_err(|e| Error::Other(e.into()))?;
        if let Some(head) = &report.head {
            let mut batch = self.ledger.batch()?;
            batch.set_meta("history_head", head)?;
            batch.commit()?;
        }
        Ok(report)
    }

    /// Commits and per-file changes matching `query`, after an incremental history update.
    pub fn search_history(&self, query: &str, top_k: usize) -> Result<Vec<HistoryHit>> {
        self.index_history(crate::history::DEFAULT_MAX_COMMITS)?;
        let history = HistoryIndex::open(&self.dir.join("history")).map_err(|e| Error::Other(e.into()))?;
        history.search(query, top_k).map_err(|e| Error::Other(e.into()))
    }

    /// Regex search over the project's indexable files (the same walk as indexing, so
    /// ignore rules apply). Does not need an index. Results are sorted by path and line.
    pub fn grep(&self, pattern: &str, paths: &[String], max_results: usize) -> Result<Vec<GrepMatch>> {
        let re = Regex::new(pattern).map_err(|e| Error::Other(e.into()))?;
        let mut builder = globset::GlobSetBuilder::new();
        for p in paths {
            builder.add(globset::Glob::new(p).map_err(|e| Error::Other(e.into()))?);
        }
        let filter = builder.build().map_err(|e| Error::Other(e.into()))?;
        let files = walk(&self.root, &self.cfg.walk)?;
        let found = Mutex::new(Vec::new());
        files
            .par_iter()
            .filter(|f| paths.is_empty() || filter.is_match(&f.path))
            .for_each(|f| {
                let Some(text) = read_text(&f.abs) else { return };
                let mut local = Vec::new();
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        let mut t: String = line.chars().take(MAX_LINE).collect();
                        if t.len() < line.len() {
                            t.push('…');
                        }
                        local.push(GrepMatch {
                            path: f.path.clone(),
                            line: i + 1,
                            text: t,
                        });
                    }
                }
                if !local.is_empty() {
                    found.lock().expect("grep mutex").extend(local);
                }
            });
        let mut out = found.into_inner().expect("grep mutex");
        out.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        out.truncate(max_results);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Config;
    use crate::retrieve::SearchOpts;
    use crate::testutil::project;

    #[test]
    fn definitions_references_and_grep() {
        let dir = project();
        std::fs::write(
            dir.path().join("pkg/sidecar/client.go"),
            "package sidecar\n\nfunc Start(name string) { run(LocateBinary(name)) }\n",
        )
        .unwrap();
        let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
        e.index(false).unwrap();

        let defs = e.definitions("LocateBinary").unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].path, "pkg/sidecar/sidecar.go");
        let refs = e.references("LocateBinary").unwrap();
        assert_eq!(refs[0].path, "pkg/sidecar/client.go");
        assert!(e.definitions("Nope").unwrap().is_empty());

        let m = e.grep(r"func \w+\(name", &[], 10).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].path.as_str(), m[0].line), ("pkg/sidecar/client.go", 3));
        let m = e.grep("func", &["pkg/rag/**".into()], 10).unwrap();
        assert_eq!(m.len(), 1);
        assert!(e.grep("(", &[], 10).is_err());

        // Pagination and thresholds.
        let all = e
            .search(
                "LocateBinary name",
                &SearchOpts {
                    top_k: 3,
                    ..Default::default()
                },
            )
            .unwrap();
        let second = e
            .search(
                "LocateBinary name",
                &SearchOpts {
                    top_k: 1,
                    offset: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(second[0], all[1]);
        let none = e
            .search(
                "LocateBinary",
                &SearchOpts {
                    min_score: Some(1e9),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(none.is_empty());
    }
}
