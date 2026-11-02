//! The `[loc]` and `[duplication]` tables of `.github/dendrite/directives.toml`.

use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

/// Where Dendrite keeps a project's quality directives, relative to the project root.
pub const DIRECTIVES_PATH: &str = ".github/dendrite/directives.toml";

/// Limits on production lines per file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LocConfig {
    /// A file with more production lines than this fails.
    pub max_production_lines: usize,
    /// A file with more production lines than this (and not more than the maximum) warns.
    pub warn_at: usize,
    /// Globs, relative to the project root, of files that are not counted.
    pub exclude: Vec<String>,
}

impl Default for LocConfig {
    fn default() -> Self {
        Self {
            max_production_lines: 500,
            warn_at: 450,
            exclude: Vec::new(),
        }
    }
}

/// Limits on the complexity of one function.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ComplexityConfig {
    /// A function with a higher cyclomatic (McCabe) complexity fails.
    pub max_cyclomatic: u32,
    /// A function with a higher cognitive (SonarSource) complexity fails.
    pub max_cognitive: u32,
    /// Globs, relative to the project root, of files that are not measured.
    pub exclude: Vec<String>,
}

impl Default for ComplexityConfig {
    fn default() -> Self {
        Self {
            max_cyclomatic: 10,
            max_cognitive: 15,
            exclude: Vec::new(),
        }
    }
}

impl ComplexityConfig {
    /// Reads the `[complexity]` table of a directives file (defaults when the file or the table is missing).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg = read_directives(path)?.complexity;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_cyclomatic == 0 || self.max_cognitive == 0 {
            bail!("complexity.max_cyclomatic and complexity.max_cognitive must be above 0");
        }
        Ok(())
    }
}

/// Limits on how a directory (package, module, crate source folder) is put together.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct StructureConfig {
    /// A directory with more modules than this (a test file counts with the file it tests) fails.
    pub max_files_per_dir: usize,
    /// A directory whose files hold more production lines than this fails.
    pub max_dir_production_lines: usize,
    /// This many modules of one directory whose names start with the same word fail: they are a sub-module.
    pub max_prefix_group: usize,
    /// Globs, relative to the project root, of files that are not looked at.
    pub exclude: Vec<String>,
}

impl Default for StructureConfig {
    fn default() -> Self {
        Self {
            max_files_per_dir: 12,
            max_dir_production_lines: 3000,
            max_prefix_group: 4,
            exclude: Vec::new(),
        }
    }
}

impl StructureConfig {
    /// Reads the `[structure]` table of a directives file (defaults when the file or the table is missing).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg = read_directives(path)?.structure;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_files_per_dir == 0 || self.max_dir_production_lines == 0 {
            bail!("structure.max_files_per_dir and structure.max_dir_production_lines must be above 0");
        }
        if self.max_prefix_group < 2 {
            bail!("structure.max_prefix_group must be at least 2");
        }
        Ok(())
    }
}

/// How much duplicated code a project tolerates, and what counts as a duplicate.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct DupConfig {
    /// Share of the counted lines that may sit in duplicated code; 0 means none.
    pub max_percent: f64,
    /// Tokens (leaves of the syntax tree) a repeated block needs to count.
    pub min_tokens: usize,
    /// Lines a repeated block needs to count.
    pub min_lines: usize,
    /// Whether test code is checked too.
    pub include_tests: bool,
}

impl Default for DupConfig {
    fn default() -> Self {
        Self {
            max_percent: 0.0,
            min_tokens: 50,
            min_lines: 5,
            include_tests: true,
        }
    }
}

impl DupConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(0.0..=100.0).contains(&self.max_percent) {
            bail!("duplication.max_percent must be between 0 and 100");
        }
        if self.min_tokens < 10 {
            bail!("duplication.min_tokens must be at least 10");
        }
        if self.min_lines < 2 {
            bail!("duplication.min_lines must be at least 2");
        }
        Ok(())
    }

    /// Reads the `[duplication]` table of a directives file (defaults when the file or the table is missing).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg = read_directives(path)?.duplication;
        cfg.validate()?;
        Ok(cfg)
    }
}

#[derive(Deserialize, Default)]
struct DirectivesFile {
    #[serde(default)]
    loc: LocConfig,
    #[serde(default)]
    duplication: DupConfig,
    #[serde(default)]
    complexity: ComplexityConfig,
    #[serde(default)]
    structure: StructureConfig,
}

/// Reads a directives file; a missing file is the defaults, an invalid one is an error.
fn read_directives(path: &Path) -> anyhow::Result<DirectivesFile> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("{}: not a valid directives file", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DirectivesFile::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

impl LocConfig {
    /// Reads the `[loc]` table of a directives file. A missing file, or a file with no such table, gives the
    /// defaults; a file that is not valid TOML, or whose limits contradict each other, is an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg = read_directives(path)?.loc;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_production_lines == 0 {
            bail!("loc.max_production_lines must be above 0");
        }
        if self.warn_at > self.max_production_lines {
            bail!(
                "loc.warn_at ({}) must not exceed loc.max_production_lines ({})",
                self.warn_at,
                self.max_production_lines
            );
        }
        Ok(())
    }
}
