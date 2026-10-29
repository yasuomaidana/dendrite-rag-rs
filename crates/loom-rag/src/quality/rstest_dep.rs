//! The rstest crate itself: a crate with Rust tests must depend on `rstest` 0.28 or later, so its tests use
//! the crate's `#[rstest]`, `#[case]`, `#[values]` and `#[fixture]` instead of functions that imitate them.

use std::path::{Path, PathBuf};

use toml::Value;

use crate::quality::rstest_check::Violation;

/// The oldest rstest the gate accepts.
pub const MIN_RSTEST: (u64, u64) = (0, 28);

/// How a manifest declares rstest.
#[derive(Debug, PartialEq, Eq)]
enum Declared {
    Missing,
    Version(u64, u64),
    /// A path or git dependency: no version to compare.
    Unversioned,
}

/// `missing_rstest` / `outdated_rstest` for the crate that owns `file`, which holds plain `#[test]` functions.
pub fn check_manifest(root: &Path, file: &Path) -> Option<Violation> {
    let manifest = crate_manifest(file)?;
    let value = read_manifest(&manifest)?;
    let rel = manifest
        .strip_prefix(root)
        .unwrap_or(&manifest)
        .to_string_lossy()
        .replace('\\', "/");
    let (kind, message) = match declared(&value, &manifest) {
        Declared::Missing => (
            "missing_rstest",
            "tests need the rstest crate: add `rstest = \"0.28.0\"` to [dev-dependencies] and use #[rstest], #[case], #[values] and #[fixture]",
        ),
        Declared::Version(major, minor) if (major, minor) < MIN_RSTEST => (
            "outdated_rstest",
            "rstest is older than 0.28: set `rstest = \"0.28.0\"` to use the current #[case], #[values], #[files], #[once], #[future] and #[timeout] options",
        ),
        _ => return None,
    };
    Some(Violation {
        kind: kind.into(),
        path: rel,
        line: 1,
        message: message.into(),
    })
}

fn read_manifest(path: &Path) -> Option<Value> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The `Cargo.toml` with a `[package]` table nearest above `file`.
fn crate_manifest(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .map(|d| d.join("Cargo.toml"))
        .find(|m| read_manifest(m).is_some_and(|v| v.get("package").is_some()))
}

fn declared(manifest: &Value, path: &Path) -> Declared {
    let dep = ["dev-dependencies", "dependencies"]
        .iter()
        .find_map(|t| manifest.get(t)?.get("rstest"));
    match dep {
        None => Declared::Missing,
        Some(Value::String(v)) => parse_version(v),
        Some(table) if table.get("workspace").and_then(Value::as_bool) == Some(true) => {
            workspace_version(path)
        }
        Some(table) => table
            .get("version")
            .and_then(Value::as_str)
            .map_or(Declared::Unversioned, parse_version),
    }
}

/// The version `[workspace.dependencies]` gives rstest, in the nearest ancestor manifest that has one.
fn workspace_version(manifest: &Path) -> Declared {
    for dir in manifest.ancestors().skip(2) {
        let Some(value) = read_manifest(&dir.join("Cargo.toml")) else {
            continue;
        };
        if let Some(dep) = value
            .get("workspace")
            .and_then(|w| w.get("dependencies"))
            .and_then(|d| d.get("rstest"))
        {
            return match dep {
                Value::String(v) => parse_version(v),
                t => t
                    .get("version")
                    .and_then(Value::as_str)
                    .map_or(Declared::Unversioned, parse_version),
            };
        }
    }
    Declared::Missing
}

/// `"0.28.0"`, `"^0.28"`, `"=0.27.1"` or `">=0.28, <0.30"` as (major, minor) of the first bound.
fn parse_version(req: &str) -> Declared {
    let digits = req.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = digits.split(|c: char| !c.is_ascii_digit());
    let mut next = || parts.next().and_then(|p| p.parse::<u64>().ok());
    match (next(), next()) {
        (Some(major), Some(minor)) => Declared::Version(major, minor),
        (Some(major), None) => Declared::Version(major, 0),
        _ => Declared::Unversioned,
    }
}
