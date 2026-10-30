//! The result of a production-lines check, as JSON, text or GitHub annotations.

use std::fmt::Write;

use serde::Serialize;

use crate::lang::Lang;
use crate::quality::check::Status;
use crate::quality::config::LocConfig;
use crate::quality::loc::LocCount;

/// Version of the JSON shape below; consumers (`dendrite allostasis gate`) check it.
pub const SCHEMA: u32 = 1;

/// One file's counts and verdict.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileReport {
    pub path: String,
    pub language: String,
    pub production: usize,
    pub test: usize,
    pub comment: usize,
    pub blank: usize,
    pub status: Status,
    pub parse_errors: bool,
}

impl FileReport {
    pub fn new(path: &str, lang: Lang, count: LocCount, status: Status) -> Self {
        Self {
            path: path.to_string(),
            language: lang.as_str().to_string(),
            production: count.production,
            test: count.test,
            comment: count.comment,
            blank: count.blank,
            status,
            parse_errors: count.parse_errors,
        }
    }
}

/// Totals over all files.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Totals {
    pub files: usize,
    pub production: usize,
    pub test: usize,
    pub warnings: usize,
    pub failures: usize,
}

/// The whole check.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocReport {
    pub schema: u32,
    pub max_production_lines: usize,
    pub warn_at: usize,
    pub totals: Totals,
    pub files: Vec<FileReport>,
}

impl LocReport {
    pub fn new(cfg: &LocConfig, files: Vec<FileReport>) -> Self {
        let count = |s: Status| files.iter().filter(|f| f.status == s).count();
        let totals = Totals {
            files: files.len(),
            production: files.iter().map(|f| f.production).sum(),
            test: files.iter().map(|f| f.test).sum(),
            warnings: count(Status::Warn),
            failures: count(Status::Fail),
        };
        Self {
            schema: SCHEMA,
            max_production_lines: cfg.max_production_lines,
            warn_at: cfg.warn_at,
            totals,
            files,
        }
    }

    /// True when no file is over the maximum.
    pub fn passed(&self) -> bool {
        self.totals.failures == 0
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The files that warn or fail, worst first, and a summary line.
    pub fn to_text(&self) -> String {
        let mut rows: Vec<&FileReport> = self.files.iter().filter(|f| f.status != Status::Ok).collect();
        rows.sort_by(|a, b| b.production.cmp(&a.production).then_with(|| a.path.cmp(&b.path)));
        let mut out = String::new();
        for f in rows {
            let tag = if f.status == Status::Fail { "FAIL" } else { "warn" };
            let _ = writeln!(
                out,
                "{tag}  {:>5} production lines  {}  ({} test)",
                f.production, f.path, f.test
            );
        }
        let t = &self.totals;
        let _ = writeln!(
            out,
            "{} files, {} production lines ({} in tests): {} over {}, {} over {}",
            t.files, t.production, t.test, t.failures, self.max_production_lines, t.warnings, self.warn_at
        );
        out
    }

    /// GitHub Actions workflow commands, one per file that warns or fails.
    pub fn to_github(&self) -> String {
        let mut out = String::new();
        for f in self.files.iter().filter(|f| f.status != Status::Ok) {
            let (level, limit) = match f.status {
                Status::Fail => ("error", self.max_production_lines),
                _ => ("warning", self.warn_at),
            };
            let _ = writeln!(
                out,
                "::{level} file={},line=1,title=Production lines::{} production lines, over {limit}",
                f.path, f.production
            );
        }
        out
    }
}
