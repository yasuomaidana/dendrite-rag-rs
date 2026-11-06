//! The retrieval methods compared: the engine's own (search and context packs per mode and
//! corpus) and the baselines in [`crate::baselines`].

use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::{Engine, Hit};
use loom_rag::retrieve::{Detail, Mode, SearchOpts, estimate_tokens};

use crate::baselines::Baselines;
use crate::metrics::Unit;

/// Hits a search method returns: enough to fill the largest budget.
const SEARCH_TOP: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Method {
    Search(Mode, CorpusFilter),
    /// Context packs, packed per budget. `map` rows count outlines: a pointer to the answer,
    /// not its text.
    Context(CorpusFilter, Detail),
    GrepAgent,
    Bm25File,
    NaiveChunks,
}

impl Method {
    pub fn label(&self) -> String {
        let corpus = |c: &CorpusFilter| match c {
            CorpusFilter::All => "all",
            CorpusFilter::Code => "code",
            CorpusFilter::Docs => "docs",
        };
        match self {
            Method::Search(m, c) => format!("search {} / {}", format!("{m:?}").to_lowercase(), corpus(c)),
            Method::Context(c, d) => format!("context {} / {}", format!("{d:?}").to_lowercase(), corpus(c)),
            Method::GrepAgent => "baseline grep-agent (whole files)".into(),
            Method::Bm25File => "baseline bm25-file (whole files)".into(),
            Method::NaiveChunks => "baseline naive-chunks (40 lines, bm25)".into(),
        }
    }

    pub fn packs_per_budget(&self) -> bool {
        matches!(self, Method::Context(..))
    }

    /// The default comparison: every engine mode and corpus, context packs, and the baselines.
    pub fn all(dense: bool) -> Vec<Method> {
        let mut out = Vec::new();
        let modes: &[Mode] = if dense {
            &[Mode::Lexical, Mode::Dense, Mode::Hybrid]
        } else {
            &[Mode::Lexical, Mode::Hybrid]
        };
        for &m in modes {
            out.push(Method::Search(m, CorpusFilter::All));
        }
        for c in [CorpusFilter::Code, CorpusFilter::Docs] {
            out.push(Method::Search(Mode::Hybrid, c));
        }
        for c in [CorpusFilter::All, CorpusFilter::Code, CorpusFilter::Docs] {
            out.push(Method::Context(c, Detail::Auto));
        }
        for d in [Detail::Full, Detail::Map] {
            out.push(Method::Context(CorpusFilter::All, d));
        }
        out.extend([Method::GrepAgent, Method::Bm25File, Method::NaiveChunks]);
        out
    }

    /// Ranked units for `query`; `budget` is used by context packs only.
    pub fn run(
        &self,
        engine: &Engine,
        baselines: &Baselines,
        query: &str,
        budget: usize,
    ) -> anyhow::Result<Vec<Unit>> {
        Ok(match *self {
            Method::Search(mode, corpus) => units(&engine.search(
                query,
                &SearchOpts {
                    top_k: SEARCH_TOP,
                    mode,
                    corpus,
                    ..SearchOpts::default()
                },
            )?),
            Method::Context(corpus, detail) => {
                units(&engine.context_with(query, budget, corpus, detail)?.spans)
            }
            Method::GrepAgent => baselines.grep(query),
            Method::Bm25File => baselines.bm25_files(query)?,
            Method::NaiveChunks => baselines.naive_chunks(query)?,
        })
    }
}

fn units(hits: &[Hit]) -> Vec<Unit> {
    hits.iter()
        .map(|h| Unit {
            path: h.path.clone(),
            also: h.duplicates.clone(),
            tokens: estimate_tokens(&h.content),
        })
        .collect()
}
