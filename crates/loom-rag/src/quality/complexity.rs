//! Functions above the cyclomatic or cognitive complexity limits (`docs/specs/quality.md` § complexity).
//!
//! The measuring is `dendrite-core`'s; this module walks the tree, applies the limits and reports. Test code is
//! measured like any other.

use std::fmt::Write;
use std::path::Path;

use dendrite_core::complexity::{FunctionMetrics, complexity};
use rayon::prelude::*;
use serde::Serialize;

use crate::lang::Lang;
use crate::quality::config::ComplexityConfig;
use crate::walk::{WalkOptions, walk};

/// Version of the JSON shape below; consumers (`dendrite allostasis gate`) check it.
pub const SCHEMA: u32 = 1;

/// A function above a limit.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub language: String,
    pub symbol: String,
    pub start_line: usize,
    pub end_line: usize,
    pub cyclomatic: u32,
    pub cognitive: u32,
}

/// What was measured.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Totals {
    pub files: usize,
    pub functions: usize,
    pub failures: usize,
}

/// The whole check: the limits, the totals and the functions over them (worst first).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ComplexityReport {
    pub schema: u32,
    pub max_cyclomatic: u32,
    pub max_cognitive: u32,
    pub totals: Totals,
    pub findings: Vec<Finding>,
}

/// The functions of one file, and those of them over the limits.
fn measure(
    path: &str,
    lang: Lang,
    metrics: Vec<FunctionMetrics>,
    cfg: &ComplexityConfig,
) -> (usize, Vec<Finding>) {
    let total = metrics.len();
    let over = metrics
        .into_iter()
        .filter(|m| m.cyclomatic > cfg.max_cyclomatic || m.cognitive > cfg.max_cognitive)
        .map(|m| Finding {
            path: path.to_string(),
            language: lang.as_str().to_string(),
            symbol: m.symbol,
            start_line: m.start_line,
            end_line: m.end_line,
            cyclomatic: m.cyclomatic,
            cognitive: m.cognitive,
        })
        .collect();
    (total, over)
}

/// Measures every function of every file under `root` that `dendrite-core` has a grammar for (gitignore-aware,
/// `target/`, `node_modules/` and the like skipped) and collects those over the limits.
pub fn check_complexity(root: &Path, cfg: &ComplexityConfig) -> anyhow::Result<ComplexityReport> {
    let opts = WalkOptions {
        max_file_bytes: u64::MAX,
        exclude: cfg.exclude.clone(),
    };
    let files = walk(root, &opts)?;
    let rows: Vec<(usize, Vec<Finding>)> = files
        .par_iter()
        .filter_map(|f| {
            let core = Lang::core(f.lang)?;
            let bytes = std::fs::read(&f.abs).ok()?;
            let metrics = complexity(core, &String::from_utf8_lossy(&bytes));
            Some(measure(&f.path, f.lang, metrics, cfg))
        })
        .collect();
    Ok(ComplexityReport::new(cfg, rows))
}

impl ComplexityReport {
    fn new(cfg: &ComplexityConfig, rows: Vec<(usize, Vec<Finding>)>) -> Self {
        let mut findings: Vec<Finding> = rows.iter().flat_map(|(_, f)| f.clone()).collect();
        findings.sort_by(|a, b| {
            (b.cyclomatic.max(b.cognitive))
                .cmp(&a.cyclomatic.max(a.cognitive))
                .then_with(|| (&a.path, a.start_line).cmp(&(&b.path, b.start_line)))
        });
        Self {
            schema: SCHEMA,
            max_cyclomatic: cfg.max_cyclomatic,
            max_cognitive: cfg.max_cognitive,
            totals: Totals {
                files: rows.len(),
                functions: rows.iter().map(|(n, _)| n).sum(),
                failures: findings.len(),
            },
            findings,
        }
    }

    /// True when no function is over a limit.
    pub fn passed(&self) -> bool {
        self.totals.failures == 0
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The functions over the limits, worst first, and a summary line.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = writeln!(
                out,
                "FAIL  cyclomatic {:>3}  cognitive {:>3}  {}:{} {}",
                f.cyclomatic, f.cognitive, f.path, f.start_line, f.symbol
            );
        }
        let t = &self.totals;
        let _ = writeln!(
            out,
            "{} files, {} functions: {} over cyclomatic {} or cognitive {}",
            t.files, t.functions, t.failures, self.max_cyclomatic, self.max_cognitive
        );
        out
    }

    /// GitHub Actions workflow commands, one per function over a limit.
    pub fn to_github(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = writeln!(
                out,
                "::error file={},line={},endLine={},title=Complexity::{} has cyclomatic {} (limit {}) and cognitive {} (limit {})",
                f.path,
                f.start_line,
                f.end_line,
                f.symbol,
                f.cyclomatic,
                self.max_cyclomatic,
                f.cognitive,
                self.max_cognitive
            );
        }
        out
    }
}

#[cfg(test)]
mod tests;
