//! SWE-bench Verified instance structures and loading (DRAG-54).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Leakage classification of an issue description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakageClass {
    /// Mentions a gold file: its full path, basename or dotted module path.
    PathExplicit,
    /// Contains stack trace lines or code identifiers in backticks.
    TracebackSymbol,
    /// Natural-language description without explicit paths or traces.
    ///
    /// Serialized as `opaque_nl`; the legacy `opaque` spelling is still accepted on load.
    #[serde(rename = "opaque_nl", alias = "opaque")]
    Opaque,
}

impl LeakageClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PathExplicit => "path_explicit",
            Self::TracebackSymbol => "traceback_symbol",
            Self::Opaque => "opaque_nl",
        }
    }
}

/// A SWE-bench Verified instance with deterministic query variants and gold files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweInstance {
    pub instance_id: String,
    pub repo: String,
    pub base_commit: String,
    #[serde(default)]
    pub created_at: Option<String>,
    pub expected_files: Vec<String>,
    pub q1: String,
    pub q2: String,
    pub q3: String,
    pub leakage_class: LeakageClass,
    #[serde(default)]
    pub stratified_120: bool,
}

/// Loads SWE-bench instances from a JSON file.
pub fn load_instances(path: &Path) -> anyhow::Result<Vec<SweInstance>> {
    let content = std::fs::read_to_string(path)?;
    let instances: Vec<SweInstance> = serde_json::from_str(&content)?;
    Ok(instances)
}
