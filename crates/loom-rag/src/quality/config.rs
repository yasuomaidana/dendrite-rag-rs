//! The `[loc]` table of `.github/dendrite/directives.toml`.

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

#[derive(Deserialize, Default)]
struct DirectivesFile {
    #[serde(default)]
    loc: LocConfig,
}

impl LocConfig {
    /// Reads the `[loc]` table of a directives file. A missing file, or a file with no such table, gives the
    /// defaults; a file that is not valid TOML, or whose limits contradict each other, is an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let cfg = match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str::<DirectivesFile>(&text)
                    .with_context(|| format!("{}: not a valid directives file", path.display()))?
                    .loc
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
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
