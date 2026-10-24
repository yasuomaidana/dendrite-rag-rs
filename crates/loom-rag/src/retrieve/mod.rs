//! Hybrid search and context packing (`docs/specs/retrieval.md` § Fusion, § Context packing
//! and § Corpora).
//!
//! Two pipelines share the stores: [`code`] ranks source code with lexical, dense and
//! symbol-name rankings and expands context packs through the symbol graph; [`docs`] ranks
//! documentation with lexical and dense rankings only. A query names its corpus with
//! [`CorpusFilter`]; `all` keeps code and documentation in one ranking for search, and
//! context packs over both run each pipeline on its own corpus ([`mixed`]).

mod code;
mod docs;
mod mixed;
mod pack;
mod route;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::corpus::{Corpus, CorpusFilter};
use crate::engine::{Engine, Error, Hit, Result};
use crate::symbols::SymbolIndex;

pub use mixed::DEFAULT_DOCS_SHARE;
use pack::{dedup, outline, pack};
pub use pack::{estimate_tokens, signature};
pub use route::Probe;

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
    /// Which corpora to search (code, documentation, or both).
    pub corpus: CorpusFilter,
    /// Glob patterns over project-relative paths; results must match one of them.
    pub paths: Vec<String>,
    /// Drop hits scoring below this (scores are mode-specific: RRF, BM25 or cosine).
    pub min_score: Option<f32>,
    /// Skip this many hits (pagination).
    pub offset: usize,
    /// Fusion weights for this query instead of the project's (the eval sweep uses it).
    #[serde(skip)]
    pub weights: Option<FusionWeights>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            top_k: 10,
            mode: Mode::Hybrid,
            corpus: CorpusFilter::All,
            paths: Vec::new(),
            min_score: None,
            offset: 0,
            weights: None,
        }
    }
}

/// How much of each span a context pack carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    /// The text of the retrieved spans; spans pulled in through the symbol graph as
    /// outlines (doc comment or heading and declaration).
    #[default]
    Auto,
    /// The text of every span.
    Full,
    /// Outlines only: a map of where the answer may be, fetched with `chunks.get`.
    Map,
}

/// Spans packed for a prompt, best first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPack {
    pub spans: Vec<Hit>,
    /// Estimated tokens of all spans.
    pub tokens: usize,
    /// The reranker found nothing likely to answer the query (`[rerank] abstain`), so the
    /// pack is empty instead of holding distractors.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abstained: bool,
    #[serde(default)]
    pub timings: Timings,
}

/// Where a query spent its time, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timings {
    pub lexical_ms: u64,
    /// Query embedding and the vector search.
    pub dense_ms: u64,
    /// Symbol-name ranking. The first query after an index change also builds the symbol
    /// index, counted in the first stage that needs it (a corpus filter needs it too).
    pub symbol_ms: u64,
    pub rerank_ms: u64,
    /// Symbol-graph expansion and PageRank (code context packs only).
    pub expand_ms: u64,
    pub total_ms: u64,
    /// Searches that ran the reranker with an abstention threshold, and those that
    /// abstained: a pack abstains when every such search did.
    #[serde(skip)]
    pub(crate) gated: u32,
    #[serde(skip)]
    pub(crate) abstained: u32,
}

impl Timings {
    /// Whether every reranked search of the query abstained (`[rerank] abstain`).
    pub fn abstained(&self) -> bool {
        self.gated > 0 && self.abstained == self.gated
    }
}

/// Weights of the hybrid fusion (`[fusion]` in the project settings, `docs/specs/retrieval.md`
/// § Fusion). Unset fields take the defaults of the embedder's family.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FusionWeights {
    pub lexical: f32,
    pub dense: f32,
    pub symbol: f32,
    /// How much code in the communities whose summaries match best is raised (0 = off).
    pub community: f32,
}

impl Default for FusionWeights {
    fn default() -> Self {
        Self {
            lexical: 1.0,
            dense: 1.0,
            symbol: 1.5,
            community: 0.0,
        }
    }
}

impl FusionWeights {
    /// Defaults for an embedder, from the weight sweeps in eval/RESULTS.md (DRAG-26).
    /// Families without a sweep keep the general defaults.
    pub fn for_model(model_id: Option<&str>) -> Self {
        let model = model_id.unwrap_or_default();
        if model.starts_with("model2vec:") {
            // Static embeddings are weak on their own: lean on BM25.
            Self {
                lexical: 1.0,
                dense: 0.5,
                symbol: 1.0,
                community: 0.0,
            }
        } else if model.contains("jina-embeddings-v2-base-code") {
            Self {
                lexical: 1.5,
                dense: 1.0,
                symbol: 2.0,
                community: 0.0,
            }
        } else {
            Self::default()
        }
    }
}

/// Fusion weights a project sets; each unset field keeps the embedder's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FusionConfig {
    pub lexical: Option<f32>,
    pub dense: Option<f32>,
    pub symbol: Option<f32>,
    pub community: Option<f32>,
}

impl FusionConfig {
    pub fn resolve(&self, base: FusionWeights) -> FusionWeights {
        FusionWeights {
            lexical: self.lexical.unwrap_or(base.lexical),
            dense: self.dense.unwrap_or(base.dense),
            symbol: self.symbol.unwrap_or(base.symbol),
            community: self.community.unwrap_or(base.community),
        }
    }
}

/// `(chunk_id, score)`, best first.
type Ranking = Vec<(String, f32)>;

/// Candidates each ranking contributes before fusion.
const CANDIDATES: usize = 50;
/// How many more candidates a ranking fetches when a corpus filter discards some of them.
const FILTER_OVERSAMPLE: usize = 4;
/// Fused hits sent to the reranker.
const RERANK_TOP: usize = 30;
/// Hits a context pack is built from.
const CONTEXT_HITS: usize = 20;

fn ms_since(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

impl Engine {
    /// The symbol index, built from the ledger on first use after each index change.
    pub fn symbol_index(&self) -> Result<Arc<SymbolIndex>> {
        if let Some(idx) = self.symbols.read().expect("symbols lock").as_ref() {
            return Ok(idx.clone());
        }
        let mut idx = SymbolIndex::default();
        self.ledger.for_each_chunk(&mut |c| idx.add(c))?;
        idx.finish();
        let idx = Arc::new(idx);
        *self.symbols.write().expect("symbols lock") = Some(idx.clone());
        Ok(idx)
    }

    /// Ranked hits for `query`.
    pub fn search(&self, query: &str, opts: &SearchOpts) -> Result<Vec<Hit>> {
        self.search_timed(query, opts, &mut Timings::default())
    }

    /// Like [`Engine::search`], adding the time of each stage to `t`.
    pub fn search_timed(&self, query: &str, opts: &SearchOpts, t: &mut Timings) -> Result<Vec<Hit>> {
        let started = Instant::now();
        let page = opts.offset + opts.top_k;
        let wanted = if opts.paths.is_empty() {
            page
        } else {
            CANDIDATES.max(page * 3)
        };
        let n = CANDIDATES.max(wanted);
        let w = opts.weights.unwrap_or_else(|| self.fusion_weights());
        let fused = match opts.corpus {
            CorpusFilter::Docs => self.docs_ranking(query, n, opts.mode, w, t)?,
            corpus => self.code_ranking(query, n, opts.mode, w, corpus, t)?,
        };
        let mut hits = dedup(self.hits(&fused[..fused.len().min(n)])?);
        if !opts.paths.is_empty() {
            let mut b = GlobSetBuilder::new();
            for p in &opts.paths {
                b.add(Glob::new(p).map_err(|e| Error::Other(e.into()))?);
            }
            let set = b.build().map_err(|e| Error::Other(e.into()))?;
            hits.retain(|h| set.is_match(&h.path));
        }
        let reranked = Instant::now();
        let top = self.rerank(query, &mut hits);
        t.rerank_ms += ms_since(reranked);
        if let (Some(top), Some(floor)) = (top, self.cfg.rerank.abstain) {
            t.gated += 1;
            if top < floor {
                t.abstained += 1;
                hits.clear();
            }
        }
        if let Some(min) = opts.min_score {
            hits.retain(|h| h.score >= min);
        }
        t.total_ms = ms_since(started);
        Ok(hits.into_iter().skip(opts.offset).take(opts.top_k).collect())
    }

    /// The full text of chunks by id (the bodies behind outlines), in the order asked;
    /// unknown ids are skipped.
    pub fn chunks(&self, ids: &[String]) -> Result<Vec<Hit>> {
        let ranked: Vec<(String, f32)> = ids.iter().map(|id| (id.clone(), 0.0)).collect();
        self.hits(&ranked)
    }

    /// The project's fusion weights: `[fusion]` over the embedder's defaults.
    pub fn fusion_weights(&self) -> FusionWeights {
        let model = self.embedder.as_ref().map(|e| e.model_id());
        self.cfg.fusion.resolve(FusionWeights::for_model(model))
    }

    /// The most relevant spans for `query` from code and documentation, packed into
    /// `token_budget` estimated tokens with part of it kept for documentation ([`mixed`]).
    pub fn context(&self, query: &str, token_budget: usize) -> Result<ContextPack> {
        self.context_for(query, token_budget, CorpusFilter::All)
    }

    /// A context pack from the corpora `corpus` admits: code packs are expanded through
    /// the symbol graph ([`code`]), documentation packs are the best sections ([`docs`]), and
    /// `all` packs hold both with part of the budget kept for documentation ([`mixed`]).
    pub fn context_for(&self, query: &str, token_budget: usize, corpus: CorpusFilter) -> Result<ContextPack> {
        self.context_with(query, token_budget, corpus, Detail::Auto)
    }

    /// [`Engine::context_for`] with the spans carried at `detail`.
    pub fn context_with(
        &self,
        query: &str,
        token_budget: usize,
        corpus: CorpusFilter,
        detail: Detail,
    ) -> Result<ContextPack> {
        self.context_shared(query, token_budget, corpus, detail, DEFAULT_DOCS_SHARE)
    }

    /// [`Engine::context_with`] keeping `docs_share` of an `all` pack's budget for
    /// documentation (0 to 1; `code` and `docs` packs ignore it).
    pub fn context_shared(
        &self,
        query: &str,
        token_budget: usize,
        corpus: CorpusFilter,
        detail: Detail,
        docs_share: f32,
    ) -> Result<ContextPack> {
        let started = Instant::now();
        let mut t = Timings::default();
        let mut pack = match corpus {
            CorpusFilter::Docs => self.docs_context(query, token_budget, detail, &mut t)?,
            CorpusFilter::Code => {
                self.code_context(query, token_budget, CorpusFilter::Code, detail, &mut t)?
            }
            CorpusFilter::All => self.mixed_context(query, token_budget, detail, docs_share, &mut t)?,
        };
        t.total_ms = ms_since(started);
        if t.abstained() {
            pack.spans.clear();
            pack.tokens = 0;
            pack.abstained = true;
        }
        pack.timings = t;
        Ok(pack)
    }

    /// A reranked hit's score as a relevance probability, when a reranker is configured
    /// (the scale of `[rerank] abstain`; `loom-eval abstain-probe` calibrates it).
    pub fn rerank_probability(&self, score: f32) -> Option<f32> {
        self.reranker.as_ref().map(|r| r.probability(score))
    }

    /// Reorders the top hits with the cross-encoder when one is configured; on failure the
    /// fused order stands. Returns the best hit's relevance probability when it reranked.
    fn rerank(&self, query: &str, hits: &mut [Hit]) -> Option<f32> {
        let reranker = self.reranker.as_ref()?;
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
            return hits.first().map(|h| reranker.probability(h.score));
        }
        None
    }

    /// The `n` best BM25 matches within `corpus`.
    fn lexical_in(&self, query: &str, n: usize, corpus: CorpusFilter, t: &mut Timings) -> Result<Ranking> {
        let started = Instant::now();
        let ranked = self.lexical.search(query, fetch(n, corpus), &self.cfg.bm25)?;
        let ranked = self.keep(ranked, n, corpus)?;
        t.lexical_ms += ms_since(started);
        Ok(ranked)
    }

    /// The `n` nearest chunks within `corpus`.
    fn dense_in(&self, query: &str, n: usize, corpus: CorpusFilter, t: &mut Timings) -> Result<Ranking> {
        let started = Instant::now();
        let ranked = self.dense_ranking(query, fetch(n, corpus));
        t.dense_ms += ms_since(started);
        self.keep(ranked?, n, corpus)
    }

    /// The first `n` entries of `ranked` that belong to `corpus`. Chunks the symbol index
    /// does not know yet count as code.
    fn keep(&self, ranked: Ranking, n: usize, corpus: CorpusFilter) -> Result<Ranking> {
        if corpus == CorpusFilter::All {
            return Ok(ranked);
        }
        let symbols = self.symbol_index()?;
        Ok(ranked
            .into_iter()
            .filter(|(id, _)| corpus.admits(symbols.corpus_of(id).unwrap_or(Corpus::Code)))
            .take(n)
            .collect())
    }
}

/// Candidates to fetch so that `n` remain after filtering by `corpus`.
fn fetch(n: usize, corpus: CorpusFilter) -> usize {
    if corpus == CorpusFilter::All {
        n
    } else {
        n * FILTER_OVERSAMPLE
    }
}
