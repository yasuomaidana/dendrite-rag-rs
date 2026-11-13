//! Counts every code file under a root and judges it against the limits.

use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

use crate::quality::config::LocConfig;
use crate::quality::loc::{LocCount, count_lines, counts_as_code};
use crate::quality::report::{FileReport, LocReport};
use crate::walk::{WalkOptions, walk};

/// How a file stands against the limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    pub fn of(count: &LocCount, cfg: &LocConfig) -> Status {
        match count.production {
            n if n > cfg.max_production_lines => Status::Fail,
            n if n > cfg.warn_at => Status::Warn,
            _ => Status::Ok,
        }
    }
}

/// Counts the production lines of every code file under `root` (gitignore-aware, `target/`, `node_modules/` and
/// the like skipped) and judges each against `cfg`.
pub fn check_loc(root: &Path, cfg: &LocConfig) -> anyhow::Result<LocReport> {
    let opts = WalkOptions {
        max_file_bytes: u64::MAX,
        exclude: cfg.exclude.clone(),
    };
    let files = walk(root, &opts)?;
    let mut rows: Vec<FileReport> = files
        .par_iter()
        .filter(|f| counts_as_code(f.lang, &f.path))
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs).ok()?;
            let count = count_lines(&f.path, f.lang, &String::from_utf8_lossy(&bytes));
            Some(FileReport::new(&f.path, f.lang, count, Status::of(&count, cfg)))
        })
        .collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(LocReport::new(cfg, rows))
}
