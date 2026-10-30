//! Structural clone detection across a source tree (`docs/specs/quality.md` § Duplication).

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;
use tree_sitter::Parser;

use crate::lang::Lang;
use crate::quality::config::DupConfig;
use crate::quality::loc::{counts_as_code, is_test_path};
use crate::quality::regions::{self, Region};
use crate::quality::syntax::{Limits, Normalize, Unit, walk};
use crate::walk::{WalkOptions, walk as walk_tree};

/// One place a block occurs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloneLocation {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Blocks that are the same up to names and constants.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloneGroup {
    pub id: usize,
    pub tokens: usize,
    pub locations: Vec<CloneLocation>,
}

/// Totals of a clone check.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CloneTotals {
    pub files: usize,
    pub lines: usize,
    pub duplicated_lines: usize,
    pub percent: f64,
    pub groups: usize,
}

/// The result of a clone check.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CloneReport {
    pub schema: u32,
    pub max_percent: f64,
    pub min_lines: usize,
    pub min_tokens: usize,
    pub include_tests: bool,
    pub totals: CloneTotals,
    pub groups: Vec<CloneGroup>,
}

impl CloneReport {
    /// True when the share of duplicated lines is within the allowance.
    pub fn passed(&self) -> bool {
        self.totals.percent <= self.max_percent
    }
}

struct FileUnits {
    path: String,
    lines: Vec<bool>, // per line: holds code
    units: Vec<Unit>,
}

/// Parses one file and returns its clone candidates, leaving out test code when `include_tests` is off.
fn units_of(path: &str, lang: Lang, src: &str, cfg: &DupConfig) -> Option<FileUnits> {
    if !cfg.include_tests && is_test_path(path) {
        return None;
    }
    let mut parser = Parser::new();
    parser.set_language(&lang.grammar()?).ok()?;
    let tree = parser.parse(src, None)?;
    let skip: Vec<Range<usize>> = if cfg.include_tests {
        Vec::new()
    } else {
        regions::find(lang, src)?
            .marks
            .into_iter()
            .filter(|m| m.kind == Region::Test)
            .map(|m| m.range)
            .collect()
    };
    let lim = Limits {
        min_lines: cfg.min_lines,
        min_tokens: cfg.min_tokens,
        skip: &skip,
    };
    let mut units = Vec::new();
    walk(
        tree.root_node(),
        src.as_bytes(),
        Normalize::Structure,
        &lim,
        &mut units,
        0,
    );
    let lines = src.lines().map(|l| !l.trim().is_empty()).collect();
    Some(FileUnits {
        path: path.to_string(),
        lines,
        units,
    })
}

/// Counts the lines of code a set of line ranges covers once.
fn covered_code_lines(file: &FileUnits, ranges: &[(usize, usize)]) -> usize {
    let mut marked = vec![false; file.lines.len()];
    for &(start, end) in ranges {
        let last = end.min(file.lines.len());
        if start >= 1 && start <= last {
            marked[start - 1..last].fill(true);
        }
    }
    marked
        .iter()
        .zip(&file.lines)
        .filter(|(m, code)| **m && **code)
        .count()
}

/// Finds blocks repeated across the code files under `root` (gitignore-aware) and judges their share of the
/// code against `cfg`. Only the largest repeated blocks are reported: a repeated function is not also reported
/// as the repeated statements inside it.
pub fn check_clones(root: &Path, cfg: &DupConfig, exclude: &[String]) -> anyhow::Result<CloneReport> {
    let opts = WalkOptions {
        max_file_bytes: u64::MAX,
        exclude: exclude.to_vec(),
    };
    let files = walk_tree(root, &opts)?;
    let parsed: Vec<FileUnits> = files
        .par_iter()
        .filter(|f| f.lang.grammar().is_some() && counts_as_code(f.lang, &f.path))
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs).ok()?;
            units_of(&f.path, f.lang, &String::from_utf8_lossy(&bytes), cfg)
        })
        .collect();
    Ok(build_report(parsed, cfg))
}

fn build_report(mut parsed: Vec<FileUnits>, cfg: &DupConfig) -> CloneReport {
    parsed.sort_by(|a, b| a.path.cmp(&b.path));
    let mut by_hash: HashMap<u64, Vec<(usize, &Unit)>> = HashMap::new();
    for (fi, f) in parsed.iter().enumerate() {
        for u in &f.units {
            by_hash.entry(u.hash).or_default().push((fi, u));
        }
    }
    let mut candidates: Vec<Vec<(usize, &Unit)>> = by_hash.into_values().filter(|g| g.len() >= 2).collect();
    // Largest first, so the enclosing block wins over the blocks inside it; ties by position for stable output.
    candidates.sort_by(|a, b| {
        b[0].1
            .tokens
            .cmp(&a[0].1.tokens)
            .then_with(|| (a[0].0, a[0].1.bytes.start).cmp(&(b[0].0, b[0].1.bytes.start)))
    });

    let mut covered: Vec<Vec<Range<usize>>> = vec![Vec::new(); parsed.len()];
    let mut groups = Vec::new();
    let mut ranges: Vec<Vec<(usize, usize)>> = vec![Vec::new(); parsed.len()];
    for mut group in candidates {
        group.sort_by_key(|(fi, u)| (*fi, u.bytes.start));
        let mut kept: Vec<(usize, &Unit)> = Vec::new();
        for (fi, u) in group {
            let inside_reported = covered[fi]
                .iter()
                .any(|c| c.start < u.bytes.end && u.bytes.start < c.end);
            let overlaps_sibling = kept
                .iter()
                .any(|(kf, k)| *kf == fi && k.bytes.start < u.bytes.end && u.bytes.start < k.bytes.end);
            if !inside_reported && !overlaps_sibling {
                kept.push((fi, u));
            }
        }
        if kept.len() < 2 {
            continue;
        }
        for (fi, u) in &kept {
            covered[*fi].push(u.bytes.clone());
            ranges[*fi].push((u.start_line, u.end_line));
        }
        groups.push(CloneGroup {
            id: groups.len() + 1,
            tokens: kept[0].1.tokens,
            locations: kept
                .iter()
                .map(|(fi, u)| CloneLocation {
                    path: parsed[*fi].path.clone(),
                    start_line: u.start_line,
                    end_line: u.end_line,
                })
                .collect(),
        });
    }
    let lines: usize = parsed
        .iter()
        .map(|f| f.lines.iter().filter(|&&c| c).count())
        .sum();
    let duplicated: usize = parsed
        .iter()
        .enumerate()
        .map(|(i, f)| covered_code_lines(f, &ranges[i]))
        .sum();
    let percent = if lines == 0 {
        0.0
    } else {
        duplicated as f64 * 100.0 / lines as f64
    };
    CloneReport {
        schema: 1,
        max_percent: cfg.max_percent,
        min_lines: cfg.min_lines,
        min_tokens: cfg.min_tokens,
        include_tests: cfg.include_tests,
        totals: CloneTotals {
            files: parsed.len(),
            lines,
            duplicated_lines: duplicated,
            percent,
            groups: groups.len(),
        },
        groups,
    }
}
