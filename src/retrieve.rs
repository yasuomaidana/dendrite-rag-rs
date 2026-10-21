//! Hybrid search and context packing (`docs/specs/retrieval.md` § Fusion and § Context packing).

use std::collections::HashMap;
use std::sync::Arc;

use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Error, Hit, Result};
use crate::fusion::{RRF_K, rrf};
use crate::symbols::SymbolIndex;

/// Which rankings a search uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Lexical + dense + symbol names, fused with weighted RRF.
    #[default]
    Hybrid,
    Lexical,
    Dense,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOpts {
    pub top_k: usize,
    pub mode: Mode,
    /// Glob patterns over project-relative paths; results must match one of them.
    pub paths: Vec<String>,
    /// Drop hits scoring below this (scores are mode-specific: RRF, BM25 or cosine).
    pub min_score: Option<f32>,
    /// Skip this many hits (pagination).
    pub offset: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            top_k: 10,
            mode: Mode::Hybrid,
            paths: Vec::new(),
            min_score: None,
            offset: 0,
        }
    }
}

/// Spans packed for a prompt, best first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPack {
    pub spans: Vec<Hit>,
    /// Estimated tokens of all spans.
    pub tokens: usize,
}

pub const WEIGHT_LEXICAL: f32 = 1.0;
pub const WEIGHT_DENSE: f32 = 1.0;
pub const WEIGHT_SYMBOL: f32 = 1.5;
/// Candidates each ranking contributes before fusion.
const CANDIDATES: usize = 50;
/// Fused hits sent to the reranker.
const RERANK_TOP: usize = 30;
/// Hits whose symbol-graph neighbours are pulled into a context pack.
const EXPAND_TOP: usize = 8;
const NEIGHBOURS_PER_HIT: usize = 4;
const HOP_DECAY: f32 = 0.5;
/// How much file PageRank can raise a span's score (×1 to ×1.5).
const PAGERANK_BOOST: f32 = 0.5;

/// Estimated tokens: ~4 characters per token with a 1.15 safety factor, plus a few for
/// the citation line.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() as f32 / 4.0 * 1.15).ceil() as usize + 8
}

impl Engine {
    /// The symbol index, built from the ledger on first use after each index change.
    pub fn symbol_index(&self) -> Result<Arc<SymbolIndex>> {
        if let Some(idx) = self.symbols.read().expect("symbols lock").as_ref() {
            return Ok(idx.clone());
        }
        let mut idx = SymbolIndex::default();
        self.ledger.for_each_chunk(|c| idx.add(c))?;
        idx.finish();
        let idx = Arc::new(idx);
        *self.symbols.write().expect("symbols lock") = Some(idx.clone());
        Ok(idx)
    }

    /// Ranked hits for `query`.
    pub fn search(&self, query: &str, opts: &SearchOpts) -> Result<Vec<Hit>> {
        let page = opts.offset + opts.top_k;
        let wanted = if opts.paths.is_empty() {
            page
        } else {
            CANDIDATES.max(page * 3)
        };
        let n = CANDIDATES.max(wanted);
        let fused = match opts.mode {
            Mode::Lexical => self.lexical.search(query, n)?,
            Mode::Dense => self.dense_ranking(query, n)?,
            Mode::Hybrid => {
                let lexical = self.lexical.search(query, n)?;
                // Dense retrieval is best effort in hybrid mode: an unreachable embedder
                // degrades to lexical + symbols rather than failing the query.
                let dense = match self.dense_ranking(query, n) {
                    Ok(d) => d,
                    Err(Error::Embed(_)) => Vec::new(),
                    Err(e) => return Err(e),
                };
                let symbol = self.symbol_index()?.rank_query(query, n);
                rrf(
                    &[
                        (WEIGHT_LEXICAL, &lexical),
                        (WEIGHT_DENSE, &dense),
                        (WEIGHT_SYMBOL, &symbol),
                    ],
                    RRF_K,
                )
            }
        };
        let mut hits = self.hits(&fused[..fused.len().min(n)])?;
        if !opts.paths.is_empty() {
            let mut b = GlobSetBuilder::new();
            for p in &opts.paths {
                b.add(Glob::new(p).map_err(|e| Error::Other(e.into()))?);
            }
            let set = b.build().map_err(|e| Error::Other(e.into()))?;
            hits.retain(|h| set.is_match(&h.path));
        }
        self.rerank(query, &mut hits);
        if let Some(min) = opts.min_score {
            hits.retain(|h| h.score >= min);
        }
        Ok(hits.into_iter().skip(opts.offset).take(opts.top_k).collect())
    }

    /// Reorders the top hits with the cross-encoder when one is configured; on failure the
    /// fused order stands.
    fn rerank(&self, query: &str, hits: &mut [Hit]) {
        let Some(reranker) = &self.reranker else { return };
        let top = hits.len().min(RERANK_TOP);
        let docs: Vec<String> = hits[..top]
            .iter()
            .map(|h| format!("{}\n{}", h.path, h.content))
            .collect();
        if let Ok(scores) = reranker.rerank(query, &docs) {
            for (h, s) in hits[..top].iter_mut().zip(scores) {
                h.score = s;
            }
            hits[..top].sort_by(|a, b| b.score.total_cmp(&a.score));
        }
    }

    /// The most relevant code for `query`, expanded through the symbol graph, re-weighted
    /// by personalized PageRank over the file reference graph, de-duplicated and packed
    /// into `token_budget` estimated tokens.
    pub fn context(&self, query: &str, token_budget: usize) -> Result<ContextPack> {
        let hits = self.search(
            query,
            &SearchOpts {
                top_k: 20,
                ..SearchOpts::default()
            },
        )?;
        let symbols = self.symbol_index()?;

        // 1. Expand the best hits one hop through the symbol graph.
        let mut scores: HashMap<String, f32> = hits.iter().map(|h| (h.chunk_id.clone(), h.score)).collect();
        for h in hits.iter().take(EXPAND_TOP) {
            for n in symbols.neighbours(&h.chunk_id, NEIGHBOURS_PER_HIT) {
                let s = h.score * HOP_DECAY;
                let e = scores.entry(n).or_insert(0.0);
                *e = e.max(s);
            }
        }

        // 2. Favour central files near the hits (repository-map ranking).
        let mut seeds: HashMap<String, f32> = HashMap::new();
        for h in &hits {
            *seeds.entry(h.path.clone()).or_default() += h.score;
        }
        let pagerank = symbols.personalized_pagerank(&seeds);

        let ranked: Vec<(String, f32)> = scores.into_iter().collect();
        let mut spans = self.hits(&ranked)?;
        for s in &mut spans {
            s.score *= 1.0 + PAGERANK_BOOST * pagerank.get(&s.path).copied().unwrap_or(0.0);
        }
        spans.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });

        // 3. Pack greedily, skipping spans already covered by a better one.
        let mut picked: Vec<Hit> = Vec::new();
        let mut tokens = 0;
        for s in spans {
            let covered = picked
                .iter()
                .any(|p| p.path == s.path && p.start_line <= s.start_line && p.end_line >= s.end_line);
            let cost = estimate_tokens(&s.content);
            if covered || tokens + cost > token_budget {
                continue;
            }
            tokens += cost;
            picked.push(s);
        }

        // 4. Merge spans that touch within a file, so the prompt reads continuous code.
        Ok(ContextPack {
            spans: merge_adjacent(picked),
            tokens,
        })
    }
}

fn merge_adjacent(mut spans: Vec<Hit>) -> Vec<Hit> {
    spans.sort_by(|a, b| a.path.cmp(&b.path).then(a.start_line.cmp(&b.start_line)));
    let mut out: Vec<Hit> = Vec::new();
    for s in spans {
        if let Some(last) = out.last_mut()
            && last.path == s.path
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Config;
    use crate::testutil::{HashEmbedder, project};
    use std::fs;

    fn engine(root: &std::path::Path) -> Engine {
        let mut e =
            Engine::open_with_embedder(root, Config::default(), Some(Box::new(HashEmbedder::new()))).unwrap();
        e.index(false).unwrap();
        e
    }

    fn calls_project() -> tempfile::TempDir {
        let dir = project();
        fs::write(
            dir.path().join("pkg/sidecar/client.go"),
            "package sidecar\n\n// Start launches a helper process.\nfunc Start(name string) error {\n\tpath := LocateBinary(name)\n\treturn run(path)\n}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn hybrid_fuses_lexical_dense_and_symbols() {
        let dir = calls_project();
        let e = engine(dir.path());
        let hits = e.search("LocateBinary", &SearchOpts::default()).unwrap();
        assert_eq!(
            hits[0].path, "pkg/sidecar/sidecar.go",
            "the definition ranks first"
        );
        assert!(
            hits.iter().any(|h| h.path == "pkg/sidecar/client.go"),
            "the caller is found too"
        );

        for mode in [Mode::Lexical, Mode::Dense] {
            let hits = e
                .search(
                    "index project",
                    &SearchOpts {
                        mode,
                        ..SearchOpts::default()
                    },
                )
                .unwrap();
            assert_eq!(hits[0].path, "pkg/rag/indexer.go", "{mode:?}");
        }
    }

    #[test]
    fn path_globs_filter_results() {
        let dir = calls_project();
        let e = engine(dir.path());
        let opts = SearchOpts {
            paths: vec!["**/*.md".into()],
            ..SearchOpts::default()
        };
        let hits = e.search("project", &opts).unwrap();
        assert!(!hits.is_empty() && hits.iter().all(|h| h.path.ends_with(".md")));
    }

    #[test]
    fn context_expands_through_the_symbol_graph_within_budget() {
        let dir = calls_project();
        let e = engine(dir.path());
        let pack = e.context("Start helper process", 2000).unwrap();
        let paths: Vec<&str> = pack.spans.iter().map(|s| s.path.as_str()).collect();
        assert!(paths.contains(&"pkg/sidecar/client.go"));
        assert!(
            paths.contains(&"pkg/sidecar/sidecar.go"),
            "the callee is pulled in: {paths:?}"
        );
        assert!(pack.tokens <= 2000);

        let tiny = e.context("Start helper process", 40).unwrap();
        assert!(tiny.tokens <= 40);
        assert!(tiny.spans.len() < pack.spans.len());
    }

    #[test]
    fn adjacent_spans_merge() {
        let hit = |start, end, content: &str| Hit {
            chunk_id: format!("{start}"),
            path: "a.go".into(),
            start_line: start,
            end_line: end,
            scope: vec![],
            score: start as f32,
            content: content.into(),
        };
        let merged = merge_adjacent(vec![hit(5, 9, "b"), hit(1, 4, "a"), hit(20, 22, "c")]);
        assert_eq!(merged.len(), 2);
        let first = merged.iter().find(|h| h.start_line == 1).unwrap();
        assert_eq!((first.end_line, first.content.as_str()), (9, "a\nb"));
    }

    #[test]
    fn hybrid_degrades_to_lexical_when_query_embedding_fails() {
        use std::sync::atomic::Ordering;
        let dir = calls_project();
        let embedder = HashEmbedder::new();
        embedder.fail_queries.store(true, Ordering::SeqCst);
        let mut e =
            Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(embedder))).unwrap();
        e.index(false).unwrap();
        assert!(e.has_vectors());
        let hits = e.search("LocateBinary", &SearchOpts::default()).unwrap();
        assert_eq!(hits[0].path, "pkg/sidecar/sidecar.go");
        let dense = SearchOpts {
            mode: Mode::Dense,
            ..SearchOpts::default()
        };
        assert!(matches!(e.search("LocateBinary", &dense), Err(Error::Embed(_))));
    }
}
