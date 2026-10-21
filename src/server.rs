//! The `dendrite-rag serve` method table (`docs/specs/protocol.md`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Config, Engine, Error};
use crate::retrieve::{Mode, SearchOpts};
use crate::rpc::{self, Handler, RpcError, code};

/// Serves every method, keeping one open engine per project root.
pub struct Server {
    config: Config,
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
    pub fn new(config: Config) -> Self {
        Self {
            config,
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
            let engine = Engine::open(&path, self.config.clone()).map_err(engine_error)?;
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
