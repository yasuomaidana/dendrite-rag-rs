//! `quality.loc`: production lines per file against a project's directives (`docs/specs/quality.md`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::{self, RpcError};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{
    ComplexityConfig, DupConfig, LocConfig, check_clones, check_complexity, check_loc, check_rstest,
};

/// The directives file of a request: its `config`, or the project's own.
fn directives_file(root: &Path, config: Option<String>) -> PathBuf {
    config.map_or_else(|| root.join(DIRECTIVES_PATH), PathBuf::from)
}

fn invalid(e: impl std::fmt::Display) -> RpcError {
    RpcError::invalid_params(format!("{e:#}"))
}

/// Replaces `target` with the request's value, when it gave one.
fn set<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

/// A gate's limits: the directives file's, changed by the request's own, and valid.
fn configured<C>(
    root: &Path,
    config: Option<String>,
    load: fn(&Path) -> anyhow::Result<C>,
    validate: fn(&C) -> anyhow::Result<()>,
    change: impl FnOnce(&mut C),
) -> Result<C, RpcError> {
    let mut cfg = load(&directives_file(root, config)).map_err(invalid)?;
    change(&mut cfg);
    validate(&cfg).map_err(invalid)?;
    Ok(cfg)
}

/// The report of a check, or the engine error it failed with.
fn finish<R: Serialize>(report: anyhow::Result<R>) -> Result<Value, RpcError> {
    let report = report.map_err(|e| RpcError::engine("quality", format!("{e:#}")))?;
    serde_json::to_value(report).map_err(|e| RpcError::engine("quality", e.to_string()))
}

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
    let cfg = configured(root, p.config, LocConfig::load, LocConfig::validate, |cfg| {
        set(&mut cfg.max_production_lines, p.max_production_lines);
        set(&mut cfg.warn_at, p.warn_at);
        set(&mut cfg.exclude, p.exclude);
    })?;
    finish(check_loc(root, &cfg))
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
    let cfg = configured(root, p.config, DupConfig::load, DupConfig::validate, |cfg| {
        set(&mut cfg.max_percent, p.max_percent);
        set(&mut cfg.min_lines, p.min_lines);
        set(&mut cfg.min_tokens, p.min_tokens);
        set(&mut cfg.include_tests, p.include_tests);
    })?;
    finish(check_clones(root, &cfg, &p.exclude.unwrap_or_default()))
}

#[derive(Deserialize)]
struct ComplexityParams {
    root: String,
    config: Option<String>,
    max_cyclomatic: Option<u32>,
    max_cognitive: Option<u32>,
    exclude: Option<Vec<String>>,
}

pub(super) fn complexity(params: Value) -> Result<Value, RpcError> {
    let p: ComplexityParams = rpc::params(params)?;
    let root = Path::new(&p.root);
    let cfg = configured(
        root,
        p.config,
        ComplexityConfig::load,
        ComplexityConfig::validate,
        |cfg| {
            set(&mut cfg.max_cyclomatic, p.max_cyclomatic);
            set(&mut cfg.max_cognitive, p.max_cognitive);
            set(&mut cfg.exclude, p.exclude);
        },
    )?;
    finish(check_complexity(root, &cfg))
}

#[derive(Deserialize)]
struct RstestParams {
    root: String,
    exclude: Option<Vec<String>>,
}

pub(super) fn rstest(params: Value) -> Result<Value, RpcError> {
    let p: RstestParams = rpc::params(params)?;
    finish(check_rstest(Path::new(&p.root), &p.exclude.unwrap_or_default()))
}
