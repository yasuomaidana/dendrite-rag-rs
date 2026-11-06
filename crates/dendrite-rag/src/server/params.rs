//! Parameters of the `dendrite-rag serve` methods (`docs/specs/protocol.md`).

use loom_rag::corpus::CorpusFilter;
use loom_rag::graph::EdgeKind;
use loom_rag::retrieve::{Detail, Mode};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct RootParams {
    pub(super) root: String,
}

#[derive(Deserialize)]
pub(super) struct IndexParams {
    pub(super) root: String,
    #[serde(default)]
    pub(super) full: bool,
    /// Stop embedding after this many milliseconds (the lexical index always completes).
    pub(super) embed_budget_ms: Option<u64>,
    /// Stop embedding after this many tokens (the lexical index always completes).
    pub(super) embed_budget_tokens: Option<usize>,
    /// Under a budget, embed the chunks this question matches first.
    pub(super) focus: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SearchParams {
    pub(super) root: String,
    pub(super) query: String,
    #[serde(default = "default_top_k")]
    pub(super) top_k: usize,
    #[serde(default)]
    pub(super) mode: Mode,
    #[serde(default)]
    pub(super) corpus: CorpusFilter,
    #[serde(default)]
    pub(super) paths: Vec<String>,
    pub(super) min_score: Option<f32>,
    #[serde(default)]
    pub(super) offset: usize,
}

#[derive(Deserialize)]
pub(super) struct ContextParams {
    pub(super) root: String,
    pub(super) query: String,
    #[serde(default = "default_budget")]
    pub(super) token_budget: usize,
    #[serde(default)]
    pub(super) corpus: CorpusFilter,
    #[serde(default)]
    pub(super) detail: Detail,
    /// Share of an `all` pack kept for documentation (default: the project's `[ask]`).
    pub(super) docs_share: Option<f32>,
}

#[derive(Deserialize)]
pub(super) struct ChunksParams {
    pub(super) root: String,
    pub(super) ids: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct SymbolParams {
    pub(super) root: String,
    pub(super) name: String,
}

#[derive(Deserialize)]
pub(super) struct HistoryIndexParams {
    pub(super) root: String,
    #[serde(default = "default_max_commits")]
    pub(super) max_commits: usize,
}

#[derive(Deserialize)]
pub(super) struct HistorySearchParams {
    pub(super) root: String,
    pub(super) query: String,
    #[serde(default = "default_top_k")]
    pub(super) top_k: usize,
}

pub(super) fn default_max_commits() -> usize {
    loom_rag::history::DEFAULT_MAX_COMMITS
}

#[derive(Deserialize)]
pub(super) struct GrepParams {
    pub(super) root: String,
    pub(super) pattern: String,
    #[serde(default)]
    pub(super) paths: Vec<String>,
    #[serde(default = "default_max_results")]
    pub(super) max_results: usize,
}

/// What `graph.communities` (`limit`, `min_size`) and `graph.god_nodes` (`min_in_degree`, `limit`) read.
#[derive(Deserialize)]
pub(super) struct GraphListParams {
    pub(super) root: String,
    #[serde(default = "default_communities")]
    pub(super) limit: usize,
    #[serde(default = "default_min_size")]
    pub(super) min_size: usize,
    #[serde(default = "default_god_in_degree")]
    pub(super) min_in_degree: usize,
}

#[derive(Deserialize)]
pub(super) struct PathParams {
    pub(super) root: String,
    pub(super) from: String,
    pub(super) to: String,
    #[serde(default)]
    pub(super) kinds: Vec<EdgeKind>,
    #[serde(default = "default_max_hops")]
    pub(super) max_hops: usize,
}

#[cfg(feature = "postgres")]
#[derive(Deserialize)]
pub(super) struct PushParams {
    pub(super) root: String,
    #[serde(default)]
    pub(super) dry_run: bool,
}

#[derive(Deserialize)]
pub(super) struct AskParams {
    pub(super) root: String,
    #[serde(flatten)]
    pub(super) request: crate::ask::AskRequest,
}

#[derive(Deserialize)]
pub(super) struct RankParams {
    pub(super) root: String,
    pub(super) query: String,
    pub(super) documents: Vec<loom_rag::rank::Document>,
    #[serde(default)]
    pub(super) top_k: usize,
}

#[derive(Deserialize)]
pub(super) struct ExplainParams {
    pub(super) root: String,
    pub(super) target: String,
}

pub(super) fn default_communities() -> usize {
    50
}

pub(super) fn default_min_size() -> usize {
    2
}

pub(super) fn default_god_in_degree() -> usize {
    loom_rag::graph::query::GOD_NODE_IN_DEGREE
}

pub(super) fn default_max_hops() -> usize {
    6
}

pub(super) fn default_top_k() -> usize {
    10
}

pub(super) fn default_budget() -> usize {
    4000
}

pub(super) fn default_max_results() -> usize {
    200
}
