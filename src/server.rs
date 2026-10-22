//! The `dendrite-rag serve` method table (`docs/specs/protocol.md`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Config, Engine, Error};
use crate::graph::EdgeKind;
use crate::retrieve::{Mode, SearchOpts};
use crate::rpc::{self, Handler, RpcError, code};

/// Serves every method, keeping one open engine per project root.
pub struct Server {
    /// Fixed configuration for every project; `None` resolves each project's own
    /// (`.dendrite/rag.toml` and the environment).
    config: Option<Config>,
    engines: HashMap<PathBuf, Engine>,
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
    crate::history::DEFAULT_MAX_COMMITS
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
struct RankParams {
    root: String,
    query: String,
    documents: Vec<crate::rank::Document>,
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
    crate::graph::query::GOD_NODE_IN_DEGREE
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
        }
    }

    /// Each project uses its own settings ([`Config::resolve`]).
    pub fn per_project() -> Self {
        Self {
            config: None,
            engines: HashMap::new(),
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
                None => Config::resolve(&path).map_err(|e| RpcError::engine("config", format!("{e:#}")))?,
            };
            let engine = Engine::open(&path, cfg).map_err(engine_error)?;
            self.engines.insert(path.clone(), engine);
        }
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
        match method {
            "health" => Ok(rpc::health()),
            "embedders" => Ok(json!({ "backends": crate::embed::backends() })),
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
                        .index_with(p.full, budget)
                        .map_err(engine_error)?,
                )
            }
            "search" => {
                let p: SearchParams = rpc::params(params)?;
                let opts = SearchOpts {
                    top_k: p.top_k,
                    mode: p.mode,
                    paths: p.paths,
                    min_score: p.min_score,
                    offset: p.offset,
                    weights: None,
                };
                let hits = self
                    .indexed(&p.root)?
                    .search(&p.query, &opts)
                    .map_err(engine_error)?;
                Ok(json!({ "hits": hits }))
            }
            "context" => {
                let p: ContextParams = rpc::params(params)?;
                to_value(
                    self.indexed(&p.root)?
                        .context(&p.query, p.token_budget)
                        .map_err(engine_error)?,
                )
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
mod tests {
    use super::*;
    use crate::embed::Backend;
    use crate::rpc::handle_line;
    use crate::testutil::project;

    fn call(s: &mut Server, method: &str, params: Value) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        serde_json::from_str(&handle_line(s, &line).unwrap()).unwrap()
    }

    #[test]
    fn full_method_table() {
        let dir = project();
        let root = dir.path().to_str().unwrap();
        let mut cfg = Config::default();
        cfg.embed.backend = Backend::None;
        let mut s = Server::new(cfg);

        let r = call(&mut s, "search", json!({"root": root, "query": "x"}));
        assert_eq!(r["error"]["data"]["kind"], "not_indexed");
        let r = call(&mut s, "graph.communities", json!({"root": root}));
        assert_eq!(r["error"]["data"]["kind"], "not_indexed");

        let r = call(&mut s, "index", json!({"root": root}));
        assert_eq!(r["result"]["files_scanned"], 3);
        let r = call(
            &mut s,
            "search",
            json!({"root": root, "query": "LocateBinary", "top_k": 2}),
        );
        assert_eq!(r["result"]["hits"][0]["path"], "pkg/sidecar/sidecar.go");
        let r = call(
            &mut s,
            "search",
            json!({"root": root, "query": "x", "mode": "bogus"}),
        );
        assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
        let r = call(
            &mut s,
            "context",
            json!({"root": root, "query": "LocateBinary", "token_budget": 500}),
        );
        assert!(r["result"]["tokens"].as_u64().unwrap() <= 500);
        let r = call(
            &mut s,
            "symbols.definitions",
            json!({"root": root, "name": "IndexProject"}),
        );
        assert_eq!(r["result"]["hits"][0]["path"], "pkg/rag/indexer.go");
        let r = call(
            &mut s,
            "rank",
            json!({"root": root, "query": "sandbox browser", "documents": [
                {"id": "a", "text": "git commits"}, {"id": "b", "text": "browser sandbox"}]}),
        );
        assert_eq!(r["result"]["ranked"][0]["id"], "b");
        let r = call(&mut s, "graph.communities", json!({"root": root, "min_size": 1}));
        assert!(!r["result"]["communities"].as_array().unwrap().is_empty());
        let r = call(
            &mut s,
            "graph.god_nodes",
            json!({"root": root, "min_in_degree": 1}),
        );
        assert!(r["result"]["nodes"].is_array());
        let r = call(
            &mut s,
            "graph.path",
            json!({"root": root, "from": "pkg/rag/indexer.go", "to": "pkg/sidecar/sidecar.go", "kinds": ["call"]}),
        );
        assert_eq!(r["result"]["level"], "file");
        let r = call(
            &mut s,
            "graph.path",
            json!({"root": root, "from": "a", "to": "b", "kinds": ["bogus"]}),
        );
        assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
        let r = call(
            &mut s,
            "graph.explain",
            json!({"root": root, "target": "IndexProject"}),
        );
        assert_eq!(r["result"]["matches"][0]["kind"], "symbol");
        let r = call(&mut s, "grep", json!({"root": root, "pattern": "^func"}));
        assert_eq!(r["result"]["matches"].as_array().unwrap().len(), 2);
        let r = call(&mut s, "grep", json!({"root": root, "pattern": "("}));
        assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
        let r = call(&mut s, "status", json!({"root": root}));
        assert_eq!(r["result"]["files"], 3);
        let r = call(&mut s, "purge", json!({"root": root}));
        assert_eq!(r["result"]["removed"], true);
        assert!(!dir.path().join(".dendrite/rag").exists());

        let r = call(&mut s, "status", json!({"root": "/definitely/missing"}));
        assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
        let r = call(&mut s, "status", json!({}));
        assert_eq!(r["error"]["code"], code::INVALID_PARAMS);
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::rpc::handle_line;

    fn call(s: &mut Server, method: &str, params: Value) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        serde_json::from_str(&handle_line(s, &line).unwrap()).unwrap()
    }

    #[test]
    fn embedders_and_project_settings() {
        let mut s = Server::per_project();
        let r = call(&mut s, "embedders", json!({}));
        let names: Vec<_> = r["result"]["backends"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["backend"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"ollama".to_string()) && names.contains(&"hf".to_string()));

        let dir = crate::testutil::project();
        std::fs::create_dir_all(dir.path().join(".dendrite")).unwrap();
        std::fs::write(
            dir.path().join(crate::settings::FILE),
            "[embed]\nbackend = \"none\"\n",
        )
        .unwrap();
        let root = dir.path().to_str().unwrap();
        let r = call(&mut s, "settings", json!({"root": root}));
        assert_eq!(r["result"]["file_exists"], true);
        // The environment may override the backend on developer machines.
        if std::env::var("DENDRITE_RAG_EMBED_BACKEND").is_err() {
            assert_eq!(r["result"]["settings"]["embed"]["backend"], "none");
            // The project's own settings open the engine: no embedder, so indexing works offline.
            let r = call(&mut s, "index", json!({"root": root}));
            assert_eq!(r["result"]["embedded"], 0, "{r}");
        }

        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join(".dendrite")).unwrap();
        std::fs::write(
            other.path().join(crate::settings::FILE),
            "[embed]\nbackend = \"word2vec\"\n",
        )
        .unwrap();
        let r = call(&mut s, "index", json!({"root": other.path().to_str().unwrap()}));
        assert_eq!(r["error"]["data"]["kind"], "config", "{r}");
    }
}
