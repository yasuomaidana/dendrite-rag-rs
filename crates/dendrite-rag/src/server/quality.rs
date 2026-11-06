//! `quality.*`: the gates against a project's directives (`docs/specs/quality.md`). Every gate takes
//! `{root, config?}` plus its own limits, which replace the ones of the directives file.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::{self, RpcError};
use loom_rag::quality::config::DIRECTIVES_PATH;
use loom_rag::quality::{
    ComplexityConfig, DupConfig, LocConfig, StructureConfig, check_clones, check_complexity, check_loc,
    check_rstest, check_structure,
};

/// A request: the project, the directives file to read (default: the project's own) and the gate's limits.
#[derive(Deserialize)]
struct Request<L> {
    root: String,
    config: Option<String>,
    #[serde(flatten)]
    limits: L,
}

fn invalid(e: impl std::fmt::Display) -> RpcError {
    RpcError::invalid_params(format!("{e:#}"))
}

/// The request's project directory; a root that is not a directory is a usage error, not an empty project.
fn project_root(root: &str) -> Result<&Path, RpcError> {
    let root = Path::new(root);
    if root.is_dir() {
        Ok(root)
    } else {
        Err(invalid(format!("root {} is not a directory", root.display())))
    }
}

/// Replaces `target` with the request's value, when it gave one.
fn set<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

/// `overrides!(cfg, l: warn_at, exclude)` applies the request's `warn_at` and `exclude` to `cfg`.
macro_rules! overrides {
    ($cfg:ident, $l:ident: $($field:ident),+) => {
        {
            $( set(&mut $cfg.$field, $l.$field.clone()); )+
        }
    };
}

/// Runs one gate: reads its limits from the directives file, applies the request's, validates and checks.
fn gate<L: DeserializeOwned, C, R: Serialize>(
    params: Value,
    load: fn(&Path) -> anyhow::Result<C>,
    validate: fn(&C) -> anyhow::Result<()>,
    apply: fn(&L, &mut C),
    check: fn(&Path, &C, &L) -> anyhow::Result<R>,
) -> Result<Value, RpcError> {
    let request: Request<L> = rpc::params(params)?;
    let root = project_root(&request.root)?;
    let file = request
        .config
        .map_or_else(|| root.join(DIRECTIVES_PATH), PathBuf::from);
    let mut cfg = load(&file).map_err(invalid)?;
    apply(&request.limits, &mut cfg);
    validate(&cfg).map_err(invalid)?;
    let report =
        check(root, &cfg, &request.limits).map_err(|e| RpcError::engine("quality", format!("{e:#}")))?;
    serde_json::to_value(report).map_err(|e| RpcError::engine("quality", e.to_string()))
}

/// A handler for a gate whose only inputs are its limits.
macro_rules! limits_gate {
    ($name:ident, $limits:ty, $cfg:ty, $check:path, $($field:ident),+) => {
        pub(super) fn $name(params: Value) -> Result<Value, RpcError> {
            let apply = |l: &$limits, cfg: &mut $cfg| overrides!(cfg, l: $($field),+);
            gate(params, <$cfg>::load, <$cfg>::validate, apply, |root, cfg, _| $check(root, cfg))
        }
    };
}

#[derive(Deserialize)]
struct LocLimits {
    max_production_lines: Option<usize>,
    warn_at: Option<usize>,
    exclude: Option<Vec<String>>,
}

pub(super) fn loc(params: Value) -> Result<Value, RpcError> {
    let apply = |l: &LocLimits, cfg: &mut LocConfig| {
        cfg.set_limits(l.max_production_lines, l.warn_at);
        overrides!(cfg, l: exclude);
    };
    gate(
        params,
        LocConfig::load,
        LocConfig::validate,
        apply,
        |root, cfg, _| check_loc(root, cfg),
    )
}

#[derive(Deserialize)]
struct ClonesLimits {
    max_percent: Option<f64>,
    min_lines: Option<usize>,
    min_tokens: Option<usize>,
    include_tests: Option<bool>,
    exclude: Option<Vec<String>>,
}

pub(super) fn clones(params: Value) -> Result<Value, RpcError> {
    let apply = |l: &ClonesLimits, cfg: &mut DupConfig| overrides!(cfg, l: max_percent, min_lines, min_tokens, include_tests);
    let check = |root: &Path, cfg: &DupConfig, l: &ClonesLimits| {
        check_clones(root, cfg, &l.exclude.clone().unwrap_or_default())
    };
    gate(params, DupConfig::load, DupConfig::validate, apply, check)
}

#[derive(Deserialize)]
struct ComplexityLimits {
    max_cyclomatic: Option<u32>,
    max_cognitive: Option<u32>,
    exclude: Option<Vec<String>>,
}

limits_gate!(
    complexity,
    ComplexityLimits,
    ComplexityConfig,
    check_complexity,
    max_cyclomatic,
    max_cognitive,
    exclude
);

#[derive(Deserialize)]
struct StructureLimits {
    max_files_per_dir: Option<usize>,
    max_dir_production_lines: Option<usize>,
    max_prefix_group: Option<usize>,
    require_docs: Option<bool>,
    docs_files: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

limits_gate!(
    structure,
    StructureLimits,
    StructureConfig,
    check_structure,
    max_files_per_dir,
    max_dir_production_lines,
    max_prefix_group,
    require_docs,
    docs_files,
    exclude
);

#[derive(Deserialize)]
struct RstestLimits {
    exclude: Option<Vec<String>>,
}

pub(super) fn rstest(params: Value) -> Result<Value, RpcError> {
    let request: Request<RstestLimits> = rpc::params(params)?;
    let report = check_rstest(
        project_root(&request.root)?,
        &request.limits.exclude.unwrap_or_default(),
    )
    .map_err(|e| RpcError::engine("quality", format!("{e:#}")))?;
    serde_json::to_value(report).map_err(|e| RpcError::engine("quality", e.to_string()))
}
