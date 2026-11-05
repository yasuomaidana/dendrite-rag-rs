//! Directories that are a pile of files instead of a structure (`docs/specs/quality.md` § structure).
//!
//! A file under the line limit is not enough: a package of forty files, or a dozen files called
//! `commit_execute_*`, `commit_push_*`, `commit_message_*`, is one concern per file name and no grouping.
//! Three rules look at each directory: how many modules it holds, how many production lines, and whether
//! several modules share a leading word (a sub-module waiting to be made).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

use crate::quality::config::StructureConfig;
use crate::quality::docs;
use crate::quality::loc::{count_lines, counts_as_code, is_test_path};
use crate::walk::{SourceFile, WalkOptions, walk};

/// Version of the JSON shape below; consumers (`dendrite allostasis gate`) check it.
pub const SCHEMA: u32 = 1;

/// A directory that breaks a rule.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    /// The directory, relative to the root (`.` for the root itself).
    pub path: String,
    /// `files`, `lines` or `prefix`.
    pub rule: String,
    pub value: usize,
    pub limit: usize,
    /// The files that make the case (all of them for `prefix`, the modules for `files`).
    pub files: Vec<String>,
    pub message: String,
}

/// What was looked at.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Totals {
    pub dirs: usize,
    pub files: usize,
    pub failures: usize,
}

/// The whole check.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StructureReport {
    pub schema: u32,
    pub max_files_per_dir: usize,
    pub max_dir_production_lines: usize,
    pub max_prefix_group: usize,
    pub totals: Totals,
    pub findings: Vec<Finding>,
}

/// One code file: where it is and how many production lines it holds.
pub struct Unit {
    pub path: String,
    pub production: usize,
}

/// The module a file belongs to: its name without extension, `_test` suffix or `test_` prefix, so a test sits
/// with the code it tests.
pub(crate) fn stem(file: &str) -> String {
    let name = file.rsplit_once('.').map_or(file, |(n, _)| n);
    let name = ["_tests", "_test", ".test", ".spec"]
        .iter()
        .find_map(|s| name.strip_suffix(s))
        .unwrap_or(name);
    name.strip_prefix("test_").unwrap_or(name).to_string()
}

/// The directory of `path` and its file name.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or((".", path))
}

/// The first word of a module name: `commit_execute_push` is in the `commit` group.
fn prefix(stem: &str) -> &str {
    stem.split('_').next().unwrap_or(stem)
}

/// Judges the directories of `units` against `cfg`. Pure: no files are read.
pub fn analyze(units: &[Unit], cfg: &StructureConfig) -> (usize, Vec<Finding>) {
    let mut dirs: BTreeMap<&str, Vec<&Unit>> = BTreeMap::new();
    for unit in units {
        dirs.entry(split(&unit.path).0).or_default().push(unit);
    }
    let findings = dirs
        .iter()
        .flat_map(|(dir, members)| judge(dir, members, cfg))
        .collect();
    (dirs.len(), findings)
}

fn judge(dir: &str, members: &[&Unit], cfg: &StructureConfig) -> Vec<Finding> {
    let mut modules: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in members {
        modules
            .entry(stem(split(&m.path).1))
            .or_default()
            .push(m.path.clone());
    }
    let mut out = size_findings(dir, members, &modules, cfg);
    out.extend(prefix_findings(dir, &modules, cfg));
    out
}

fn size_findings(
    dir: &str,
    members: &[&Unit],
    modules: &BTreeMap<String, Vec<String>>,
    cfg: &StructureConfig,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let lines: usize = members
        .iter()
        .filter(|m| !is_test_path(&m.path))
        .map(|m| m.production)
        .sum();
    if modules.len() > cfg.max_files_per_dir {
        out.push(Finding {
            path: dir.to_string(),
            rule: "files".into(),
            value: modules.len(),
            limit: cfg.max_files_per_dir,
            files: modules.values().flatten().cloned().collect(),
            message: format!(
                "{} modules in one directory; group them into sub-modules by what they are about",
                modules.len()
            ),
        });
    }
    if lines > cfg.max_dir_production_lines {
        out.push(Finding {
            path: dir.to_string(),
            rule: "lines".into(),
            value: lines,
            limit: cfg.max_dir_production_lines,
            files: members.iter().map(|m| m.path.clone()).collect(),
            message: format!("{lines} production lines in one directory; split it into modules with one responsibility each"),
        });
    }
    out
}

fn prefix_findings(
    dir: &str,
    modules: &BTreeMap<String, Vec<String>>,
    cfg: &StructureConfig,
) -> Vec<Finding> {
    let mut groups: BTreeMap<&str, BTreeSet<&String>> = BTreeMap::new();
    for stem in modules.keys() {
        groups.entry(prefix(stem)).or_default().insert(stem);
    }
    groups
        .into_iter()
        .filter(|(_, stems)| stems.len() >= cfg.max_prefix_group)
        .map(|(word, stems)| Finding {
            path: dir.to_string(),
            rule: "prefix".into(),
            value: stems.len(),
            limit: cfg.max_prefix_group,
            files: stems.iter().flat_map(|s| modules[*s].clone()).collect(),
            message: format!(
                "{} modules start with `{word}`; move them into a `{word}` sub-module",
                stems.len()
            ),
        })
        .collect()
}

/// Looks at every code file under `root` (gitignore-aware, `target/`, `node_modules/` and the like skipped).
pub fn check_structure(root: &Path, cfg: &StructureConfig) -> anyhow::Result<StructureReport> {
    let opts = WalkOptions {
        max_file_bytes: u64::MAX,
        exclude: cfg.exclude.clone(),
    };
    let files = walk(root, &opts)?;
    let units: Vec<Unit> = files
        .par_iter()
        .filter(|f| counts_as_code(f.lang, &f.path))
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs).ok()?;
            let count = count_lines(&f.path, f.lang, &String::from_utf8_lossy(&bytes));
            Some(Unit {
                path: f.path.clone(),
                production: count.production,
            })
        })
        .collect();
    let (dirs, mut findings) = analyze(&units, cfg);
    if cfg.require_docs {
        findings.extend(documentation_findings(root, &files, cfg));
    }
    Ok(StructureReport::new(cfg, units.len(), dirs, findings))
}

/// The `docs` findings: the units under `root` without their documentation files.
fn documentation_findings(root: &Path, files: &[SourceFile], cfg: &StructureConfig) -> Vec<Finding> {
    let paths: BTreeSet<String> = files.iter().map(|f| f.path.clone()).collect();
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap_or_default();
    let manifests: Vec<String> = paths
        .iter()
        .filter(|p| docs::is_manifest(p, &read))
        .cloned()
        .collect();
    docs::missing_docs(&manifests, &paths, &cfg.docs_files)
}

impl StructureReport {
    fn new(cfg: &StructureConfig, files: usize, dirs: usize, findings: Vec<Finding>) -> Self {
        Self {
            schema: SCHEMA,
            max_files_per_dir: cfg.max_files_per_dir,
            max_dir_production_lines: cfg.max_dir_production_lines,
            max_prefix_group: cfg.max_prefix_group,
            totals: Totals {
                dirs,
                files,
                failures: findings.len(),
            },
            findings,
        }
    }

    /// True when no directory breaks a rule.
    pub fn passed(&self) -> bool {
        self.totals.failures == 0
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// One line per finding and a summary.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = writeln!(
                out,
                "FAIL  {:<6} {:>5} (limit {})  {}: {}",
                f.rule, f.value, f.limit, f.path, f.message
            );
        }
        let t = &self.totals;
        let _ = writeln!(
            out,
            "{} directories, {} files: {} findings",
            t.dirs, t.files, t.failures
        );
        out
    }

    /// GitHub Actions workflow commands, one per finding.
    pub fn to_github(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = writeln!(
                out,
                "::error file={},title=Structure ({})::{}",
                f.files.first().unwrap_or(&f.path),
                f.rule,
                f.message
            );
        }
        out
    }
}

#[cfg(test)]
mod tests;
