//! The `dendrite-rag serve` method table (`docs/specs/protocol.md`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::rpc::{self, Handler, RpcError, code};
use crate::settings::{AskSettings, Settings, valid_docs_share};
use loom_rag::corpus::CorpusFilter;
use loom_rag::engine::{Config, Engine, Error};
use loom_rag::graph::EdgeKind;
use loom_rag::retrieve::{Detail, Mode, SearchOpts, Timings};

/// Serves every method, keeping one open engine per project root.
pub struct Server {
    /// Fixed configuration for every project; `None` resolves each project's own
    /// (`.dendrite/rag.toml` and the environment).
    config: Option<Config>,
    engines: HashMap<PathBuf, Engine>,
    /// `[ask]` of each project opened with its own settings.
    asks: HashMap<PathBuf, AskSettings>,
    /// When each open engine was last used (end of the last request that touched it).
    last_used: HashMap<PathBuf, Instant>,
    /// Roots the current request used; stamped into `last_used` when it finishes.
    touched: Vec<PathBuf>,
    /// An engine idle for this long is closed, releasing the project's exclusive index lock so
    /// another process can open it. `None` keeps engines open for the life of the process.
    engine_idle: Option<Duration>,
    /// How long opening a project retries while another process holds its lock.
    open_wait: Duration,
}

/// Default idle time before a project's engine is closed (`DENDRITE_RAG_ENGINE_IDLE_SECS`, 0 = never).
const DEFAULT_ENGINE_IDLE: Duration = Duration::from_secs(5);
/// Default time to wait for another process's lock (`DENDRITE_RAG_OPEN_WAIT_SECS`).
const DEFAULT_OPEN_WAIT: Duration = Duration::from_secs(12);

fn engine_idle_from_env() -> Option<Duration> {
    match env_secs("DENDRITE_RAG_ENGINE_IDLE_SECS") {
        Some(0) => None,
        Some(n) => Some(Duration::from_secs(n)),
        None => Some(DEFAULT_ENGINE_IDLE),
    }
}

fn open_wait_from_env() -> Duration {
    env_secs("DENDRITE_RAG_OPEN_WAIT_SECS").map_or(DEFAULT_OPEN_WAIT, Duration::from_secs)
}

fn env_secs(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

/// True for the error a project's index lock produces while another process has it open.
fn is_locked(e: &Error) -> bool {
    let msg = e.to_string();
    msg.contains("already open") || msg.contains("Cannot acquire lock")
}

#[derive(Deserialize)]
struct RootParams {
    root: String,
}

#[derive(Deserialize)]
struct IndexParams {
    root: String,
    #[serde(default)]
    full: bool,
    /// Stop embedding after this many milliseconds (the lexical index always completes).
    embed_budget_ms: Option<u64>,
    /// Stop embedding after this many tokens (the lexical index always completes).
    embed_budget_tokens: Option<usize>,
    /// Under a budget, embed the chunks this question matches first.
    focus: Option<String>,
}

#[derive(Deserialize)]
struct SearchParams {
    root: String,
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    corpus: CorpusFilter,
    #[serde(default)]
    paths: Vec<String>,
    min_score: Option<f32>,
    #[serde(default)]
    offset: usize,
}

#[derive(Deserialize)]
struct ContextParams {
    root: String,
    query: String,
    #[serde(default = "default_budget")]
    token_budget: usize,
    #[serde(default)]
    corpus: CorpusFilter,
    #[serde(default)]
    detail: Detail,
    /// Share of an `all` pack kept for documentation (default: the project's `[ask]`).
    docs_share: Option<f32>,
}

#[derive(Deserialize)]
struct ChunksParams {
    root: String,
    ids: Vec<String>,
}

#[derive(Deserialize)]
struct SymbolParams {
    root: String,
    name: String,
}

#[derive(Deserialize)]
struct HistoryIndexParams {
    root: String,
    #[serde(default = "default_max_commits")]
    max_commits: usize,
}

#[derive(Deserialize)]
struct HistorySearchParams {
    root: String,
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

fn default_max_commits() -> usize {
    loom_rag::history::DEFAULT_MAX_COMMITS
}

#[derive(Deserialize)]
struct GrepParams {
    root: String,
    pattern: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

#[derive(Deserialize)]
struct CommunitiesParams {
    root: String,
    #[serde(default = "default_communities")]
    limit: usize,
    #[serde(default = "default_min_size")]
    min_size: usize,
}

#[derive(Deserialize)]
struct GodNodesParams {
    root: String,
    #[serde(default = "default_god_in_degree")]
    min_in_degree: usize,
    #[serde(default = "default_communities")]
    limit: usize,
}

#[derive(Deserialize)]
struct PathParams {
    root: String,
    from: String,
    to: String,
    #[serde(default)]
    kinds: Vec<EdgeKind>,
    #[serde(default = "default_max_hops")]
    max_hops: usize,
}

#[cfg(feature = "postgres")]
#[derive(Deserialize)]
struct PushParams {
    root: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize)]
struct AskParams {
    root: String,
    #[serde(flatten)]
    request: crate::ask::AskRequest,
}

#[derive(Deserialize)]
struct RankParams {
    root: String,
    query: String,
    documents: Vec<loom_rag::rank::Document>,
    #[serde(default)]
    top_k: usize,
}

#[derive(Deserialize)]
struct ExplainParams {
    root: String,
    target: String,
}

fn default_communities() -> usize {
    50
}

fn default_min_size() -> usize {
    2
}

fn default_god_in_degree() -> usize {
    loom_rag::graph::query::GOD_NODE_IN_DEGREE
}

fn default_max_hops() -> usize {
    6
}

fn default_top_k() -> usize {
    10
}

fn default_budget() -> usize {
    4000
}

fn default_max_results() -> usize {
    200
}

impl Server {
    /// One configuration for every project.
    pub fn new(config: Config) -> Self {
        Self {
            config: Some(config),
            engines: HashMap::new(),
            asks: HashMap::new(),
            last_used: HashMap::new(),
            touched: Vec::new(),
            engine_idle: engine_idle_from_env(),
            open_wait: open_wait_from_env(),
        }
    }

    /// Each project uses its own settings ([`crate::settings::resolve`]).
    pub fn per_project() -> Self {
        Self {
            config: None,
            engines: HashMap::new(),
            asks: HashMap::new(),
            last_used: HashMap::new(),
            touched: Vec::new(),
            engine_idle: engine_idle_from_env(),
            open_wait: open_wait_from_env(),
        }
    }

    /// Overrides the idle-close and lock-wait times (tests, embedding hosts).
    pub fn with_timing(mut self, engine_idle: Option<Duration>, open_wait: Duration) -> Self {
        self.engine_idle = engine_idle;
        self.open_wait = open_wait;
        self
    }

    /// Closes engines unused for `engine_idle`, releasing their projects' index locks.
    fn close_idle_engines(&mut self) {
        let Some(max) = self.engine_idle else { return };
        let now = Instant::now();
        let stale: Vec<PathBuf> = self
            .last_used
            .iter()
            .filter(|(_, at)| now.duration_since(**at) >= max)
            .map(|(p, _)| p.clone())
            .collect();
        for path in stale {
            self.engines.remove(&path);
            self.last_used.remove(&path);
        }
    }

    fn engine(&mut self, root: &str) -> Result<&mut Engine, RpcError> {
        let path = Path::new(root)
            .canonicalize()
            .map_err(|e| RpcError::invalid_params(format!("root {root:?}: {e}")))?;
        if !path.is_dir() {
            return Err(RpcError::invalid_params(format!(
                "root {root:?} is not a directory"
            )));
        }
        if !self.engines.contains_key(&path) {
            let cfg = match &self.config {
                Some(c) => c.clone(),
                None => {
                    let settings =
                        Settings::load(&path).map_err(|e| RpcError::engine("config", format!("{e:#}")))?;
                    self.asks.insert(path.clone(), settings.ask.clone());
                    settings.config_at(&path)
                }
            };
            // Another process may hold the project's lock; it closes idle engines on its own, so
            // wait for it for a while instead of failing.
            let deadline = Instant::now() + self.open_wait;
            let engine = loop {
                match Engine::open(&path, cfg.clone()) {
                    Ok(engine) => break engine,
                    Err(e) if is_locked(&e) && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    Err(e) => return Err(engine_error(e)),
                }
            };
            self.engines.insert(path.clone(), engine);
        }
        self.touched.push(path.clone());
        Ok(self.engines.get_mut(&path).expect("just inserted"))
    }

    /// An engine whose project has been indexed at least once.
    fn indexed(&mut self, root: &str) -> Result<&mut Engine, RpcError> {
        let engine = self.engine(root)?;
        if engine.status().map_err(engine_error)?.last_indexed.is_none() {
            return Err(RpcError::engine(
                "not_indexed",
                format!("{root} has not been indexed; call index first"),
            ));
        }
        Ok(engine)
    }
}

/// A requested documentation share outside 0 to 1 is invalid params.
fn check_docs_share(share: Option<f32>) -> Result<(), RpcError> {
    match share {
        Some(s) if !valid_docs_share(s) => Err(RpcError::invalid_params(format!(
            "docs_share must be a number from 0 to 1, got {s}"
        ))),
        _ => Ok(()),
    }
}

fn engine_error(e: Error) -> RpcError {
    RpcError::engine(e.kind(), e.to_string())
}

/// History failures (not a git repository, git missing) are reported as `kind: "git"`.
fn history_error(e: Error) -> RpcError {
    match e {
        Error::Other(e) => RpcError::engine("git", e.to_string()),
        e => engine_error(e),
    }
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(code::ENGINE_ERROR, e.to_string()))
}

impl Handler for Server {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.touched.clear();
        let result = self.dispatch(method, params);
        let now = Instant::now();
        for path in std::mem::take(&mut self.touched) {
            if self.engines.contains_key(&path) {
                self.last_used.insert(path, now);
            }
        }
        result
    }

    fn idle(&mut self) {
        self.close_idle_engines();
    }
}

impl Server {
    fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "health" => Ok(rpc::health()),
            "embedders" => Ok(json!({ "backends": loom_rag::embed::backends() })),
            "settings" => {
                let p: RootParams = rpc::params(params)?;
                let root = Path::new(&p.root);
                let file = crate::settings::Settings::path(root);
                let settings = crate::settings::Settings::load(root)
                    .map_err(|e| RpcError::engine("config", format!("{e:#}")))?;
                Ok(json!({
                    "file": file,
                    "file_exists": file.is_file(),
                    "settings": settings,
                }))
            }
            "index" => {
                let p: IndexParams = rpc::params(params)?;
                let budget = p.embed_budget_ms.map(std::time::Duration::from_millis);
                to_value(
                    self.engine(&p.root)?
                        .index_focused_budgeted(p.full, budget, p.embed_budget_tokens, p.focus.as_deref())
                        .map_err(engine_error)?,
                )
            }
            "search" => {
                let p: SearchParams = rpc::params(params)?;
                let opts = SearchOpts {
                    top_k: p.top_k,
                    mode: p.mode,
                    corpus: p.corpus,
                    paths: p.paths,
                    min_score: p.min_score,
                    offset: p.offset,
                    weights: None,
                };
                let mut timings = Timings::default();
                let hits = self
                    .indexed(&p.root)?
                    .search_timed(&p.query, &opts, &mut timings)
                    .map_err(engine_error)?;
                Ok(json!({ "hits": hits, "timings": timings }))
            }
            "context" => {
                let p: ContextParams = rpc::params(params)?;
                check_docs_share(p.docs_share)?;
                let engine = self.indexed(&p.root)?;
                let root = engine.root().to_path_buf();
                let share = self
                    .asks
                    .get(&root)
                    .cloned()
                    .unwrap_or_default()
                    .docs_share(p.docs_share);
                let engine = self.engines.get(&root).expect("opened above");
                to_value(
                    engine
                        .context_shared(&p.query, p.token_budget, p.corpus, p.detail, share)
                        .map_err(engine_error)?,
                )
            }
            "chunks.get" => {
                let p: ChunksParams = rpc::params(params)?;
                let hits = self.indexed(&p.root)?.chunks(&p.ids).map_err(engine_error)?;
                Ok(json!({ "hits": hits }))
            }
            "sync" => {
                let p: RootParams = rpc::params(params)?;
                let pending = self.engine(&p.root)?.sync().map_err(engine_error)?;
                Ok(json!({ "pending": pending }))
            }
            #[cfg(feature = "postgres")]
            "push" => {
                let p: PushParams = rpc::params(params)?;
                to_value(self.indexed(&p.root)?.push(p.dry_run).map_err(engine_error)?)
            }
            "pull" => {
                let p: RootParams = rpc::params(params)?;
                to_value(self.indexed(&p.root)?.pull().map_err(engine_error)?)
            }
            "status" => {
                let p: RootParams = rpc::params(params)?;
                to_value(self.engine(&p.root)?.status().map_err(engine_error)?)
            }
            "purge" => {
                let p: RootParams = rpc::params(params)?;
                self.engine(&p.root)?;
                let path = Path::new(&p.root)
                    .canonicalize()
                    .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let engine = self.engines.remove(&path).expect("opened above");
                engine.purge().map_err(engine_error)?;
                Ok(json!({ "removed": true }))
            }
            "symbols.definitions" | "symbols.references" => {
                let p: SymbolParams = rpc::params(params)?;
                let engine = self.indexed(&p.root)?;
                let hits = if method == "symbols.definitions" {
                    engine.definitions(&p.name)
                } else {
                    engine.references(&p.name)
                }
                .map_err(engine_error)?;
                Ok(json!({ "hits": hits }))
            }
            "history.index" => {
                let p: HistoryIndexParams = rpc::params(params)?;
                to_value(
                    self.engine(&p.root)?
                        .index_history(p.max_commits)
                        .map_err(history_error)?,
                )
            }
            "history.search" => {
                let p: HistorySearchParams = rpc::params(params)?;
                let hits = self
                    .engine(&p.root)?
                    .search_history(&p.query, p.top_k)
                    .map_err(history_error)?;
                Ok(json!({ "hits": hits }))
            }
            "ask" => {
                let p: AskParams = rpc::params(params)?;
                check_docs_share(p.request.docs_share)?;
                let gateway = loom_rag::llm::Client::new(None, None, String::new());
                let engine = self.indexed(&p.root)?;
                let root = engine.root().to_path_buf();
                let settings = self.asks.get(&root).cloned().unwrap_or_default();
                let engine = self.engines.get(&root).expect("opened above");
                to_value(crate::ask::ask(engine, &settings, &p.request, &gateway).map_err(engine_error)?)
            }
            "rank" => {
                let p: RankParams = rpc::params(params)?;
                let ranked = self
                    .engine(&p.root)?
                    .rank(&p.documents, &p.query, p.top_k)
                    .map_err(engine_error)?;
                Ok(json!({ "ranked": ranked }))
            }
            "graph.communities" => {
                let p: CommunitiesParams = rpc::params(params)?;
                let communities = self
                    .indexed(&p.root)?
                    .graph_communities(p.limit, p.min_size)
                    .map_err(engine_error)?;
                Ok(json!({ "communities": communities }))
            }
            "graph.god_nodes" => {
                let p: GodNodesParams = rpc::params(params)?;
                let nodes = self
                    .indexed(&p.root)?
                    .graph_god_nodes(p.min_in_degree, p.limit)
                    .map_err(engine_error)?;
                Ok(json!({ "nodes": nodes }))
            }
            "graph.path" => {
                let p: PathParams = rpc::params(params)?;
                to_value(
                    self.indexed(&p.root)?
                        .graph_path(&p.from, &p.to, &p.kinds, p.max_hops)
                        .map_err(engine_error)?,
                )
            }
            "graph.explain" => {
                let p: ExplainParams = rpc::params(params)?;
                let matches = self
                    .indexed(&p.root)?
                    .graph_explain(&p.target)
                    .map_err(engine_error)?;
                Ok(json!({ "matches": matches }))
            }
            "grep" => {
                let p: GrepParams = rpc::params(params)?;
                let matches = self
                    .engine(&p.root)?
                    .grep(&p.pattern, &p.paths, p.max_results)
                    .map_err(|e| match e {
                        Error::Other(e) => RpcError::invalid_params(e.to_string()),
                        e => engine_error(e),
                    })?;
                Ok(json!({ "matches": matches }))
            }
            _ => Err(RpcError::new(
                code::METHOD_NOT_FOUND,
                format!("unknown method {method}"),
            )),
        }
    }
}

#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod tests;
