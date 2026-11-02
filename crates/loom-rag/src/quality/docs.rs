//! The `docs` rule of the structure gate: every project unit explains itself and keeps a changelog.
//!
//! A unit is the repository root and every directory with a manifest of its own: a Cargo package, a Go module,
//! an npm package or a Python project. Walking is done by the caller; this module only judges paths.

use std::collections::BTreeSet;

use crate::quality::structure::Finding;

/// Manifest file names that make their directory a project unit.
const MANIFESTS: [&str; 4] = ["Cargo.toml", "go.mod", "package.json", "pyproject.toml"];

/// Whether `path` names a unit manifest. A `Cargo.toml` that only declares a workspace is not a package.
pub(crate) fn is_manifest(path: &str, contents: &dyn Fn(&str) -> String) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "Cargo.toml" => {
            let text = contents(path);
            text.contains("[package]") || !text.contains("[workspace]")
        }
        _ => MANIFESTS.contains(&name),
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

/// The findings for units that lack documentation. `manifests` are the unit manifests found; `files` is every
/// walked file; the repository root (`.`) is always a unit.
pub fn missing_docs(manifests: &[String], files: &BTreeSet<String>, required: &[String]) -> Vec<Finding> {
    let mut dirs: BTreeSet<&str> = manifests.iter().map(|m| dir_of(m)).collect();
    dirs.insert(".");
    dirs.into_iter()
        .filter_map(|dir| {
            let missing: Vec<&String> = required
                .iter()
                .filter(|name| !files.contains(&join(dir, name)))
                .collect();
            (!missing.is_empty()).then(|| finding(dir, &missing))
        })
        .collect()
}

fn join(dir: &str, name: &str) -> String {
    if dir == "." {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn finding(dir: &str, missing: &[&String]) -> Finding {
    let names: Vec<&str> = missing.iter().map(|n| n.as_str()).collect();
    Finding {
        path: dir.to_string(),
        rule: "docs".into(),
        value: missing.len(),
        limit: 0,
        files: missing.iter().map(|n| join(dir, n)).collect(),
        message: format!(
            "{} has no {}; write what it is for in the README and keep a changelog (Keep a Changelog format)",
            if dir == "." { "the repository root" } else { dir },
            names.join(" and ")
        ),
    }
}
