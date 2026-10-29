//! `quality.loc`: production lines per file against a project's directives (`docs/specs/quality.md`).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::rpc::{self, RpcError};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{DupConfig, LocConfig, check_clones, check_loc, check_rstest};

#[derive(Deserialize)]
struct LocParams {
    root: String,
    /// Directives file; default `<root>/.github/dendrite/directives.toml`.
    config: Option<String>,
    max_production_lines: Option<usize>,
    warn_at: Option<usize>,
    exclude: Option<Vec<String>>,
}

pub(super) fn loc(params: Value) -> Result<Value, RpcError> {
    let p: LocParams = rpc::params(params)?;
    let root = Path::new(&p.root);
    let file = p.config.map_or_else(|| root.join(DIRECTIVES_PATH), PathBuf::from);
    let mut cfg = LocConfig::load(&file).map_err(|e| RpcError::invalid_params(format!("{e:#}")))?;
    cfg.max_production_lines = p.max_production_lines.unwrap_or(cfg.max_production_lines);
    cfg.warn_at = p.warn_at.unwrap_or(cfg.warn_at);
    cfg.exclude = p.exclude.unwrap_or(cfg.exclude);
    cfg.validate()
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let report = check_loc(root, &cfg).map_err(|e| RpcError::engine("quality", format!("{e:#}")))?;
    serde_json::to_value(report).map_err(|e| RpcError::engine("quality", e.to_string()))
}

#[derive(Deserialize)]
struct ClonesParams {
    root: String,
    config: Option<String>,
    max_percent: Option<f64>,
    min_lines: Option<usize>,
    min_tokens: Option<usize>,
    include_tests: Option<bool>,
    exclude: Option<Vec<String>>,
}

pub(super) fn clones(params: Value) -> Result<Value, RpcError> {
    let p: ClonesParams = rpc::params(params)?;
    let root = Path::new(&p.root);
    let file = p.config.map_or_else(|| root.join(DIRECTIVES_PATH), PathBuf::from);
    let mut cfg = DupConfig::load(&file).map_err(|e| RpcError::invalid_params(format!("{e:#}")))?;
    cfg.max_percent = p.max_percent.unwrap_or(cfg.max_percent);
    cfg.min_lines = p.min_lines.unwrap_or(cfg.min_lines);
    cfg.min_tokens = p.min_tokens.unwrap_or(cfg.min_tokens);
    cfg.include_tests = p.include_tests.unwrap_or(cfg.include_tests);
    cfg.validate()
        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
    let report = check_clones(root, &cfg, &p.exclude.unwrap_or_default())
        .map_err(|e| RpcError::engine("quality", format!("{e:#}")))?;
    serde_json::to_value(report).map_err(|e| RpcError::engine("quality", e.to_string()))
}

#[derive(Deserialize)]
struct RstestParams {
    root: String,
    exclude: Option<Vec<String>>,
}

pub(super) fn rstest(params: Value) -> Result<Value, RpcError> {
    let p: RstestParams = rpc::params(params)?;
    let report = check_rstest(Path::new(&p.root), &p.exclude.unwrap_or_default())
        .map_err(|e| RpcError::engine("quality", format!("{e:#}")))?;
    serde_json::to_value(report).map_err(|e| RpcError::engine("quality", e.to_string()))
}
