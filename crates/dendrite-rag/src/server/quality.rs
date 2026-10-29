//! `quality.loc`: production lines per file against a project's directives (`docs/specs/quality.md`).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::rpc::{self, RpcError};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{LocConfig, check_loc};

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
