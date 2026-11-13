//! Database access goes through Diesel (`docs/specs/store.md` § Database access).
//!
//! Hand-written SQL (`sql_query`, `batch_execute`, `dsl::sql`) is allowed only in the `raw`
//! modules of the stores, where each statement is one the Diesel DSL cannot express, and in
//! test code. No other database crate may be used.

use std::fs;
use std::path::{Path, PathBuf};

/// Where hand-written SQL may appear: the raw modules, test fixtures and test modules.
const SQL_ALLOWED: &[&str] = &[
    "src/store/pg/raw/",
    "src/store/sqlite/raw.rs",
    "src/store/sqlite/tests.rs",
    "src/testutil.rs",
];
const RAW_SQL: &[&str] = ["sql_query", "batch_execute", "dsl::sql"].as_slice();
/// Crates that talk to a database without Diesel.
const FOREIGN: &[&str] = &[
    "postgres::",
    "rusqlite",
    "tokio_postgres",
    "sqlx",
    "sea_orm",
    "libsql",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests", "benches", "examples"] {
        if root.join(dir).is_dir() {
            rust_files(&root.join(dir), &mut files);
        }
    }
    files
        .into_iter()
        .filter(|p| !p.ends_with("tests/db_access.rs"))
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            (rel, fs::read_to_string(&p).unwrap())
        })
        .collect()
}

/// Code lines only: comments and doc comments may mention the names.
fn code(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
}

/// `file:line: word` for every code line that mentions one of `words`, outside `allowed`.
fn mentions(words: &[&str], allowed: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for (file, text) in sources() {
        if allowed.iter().any(|a| file.starts_with(a)) {
            continue;
        }
        for (n, line) in code(&text) {
            if let Some(w) = words.iter().find(|w| line.contains(**w)) {
                found.push(format!("{file}:{}: `{w}`", n + 1));
            }
        }
    }
    found
}

#[test]
fn hand_written_sql_stays_in_the_raw_modules() {
    let found = mentions(RAW_SQL, SQL_ALLOWED);
    assert!(
        found.is_empty(),
        "use the Diesel DSL, or move the statement into a raw module:\n{}",
        found.join("\n")
    );
}

#[test]
fn no_database_crate_but_diesel_is_used() {
    let mut found = mentions(FOREIGN, &[]);
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let mut in_deps = false;
    for (n, line) in manifest.lines().enumerate() {
        if line.starts_with('[') {
            in_deps = line.contains("dependencies");
        }
        let name = line.split(['=', ' ']).next().unwrap_or_default();
        if in_deps
            && [
                "postgres",
                "tokio-postgres",
                "rusqlite",
                "sqlx",
                "sea-orm",
                "libsql",
            ]
            .contains(&name)
        {
            found.push(format!("Cargo.toml:{}: `{name}`", n + 1));
        }
    }
    assert!(
        found.is_empty(),
        "database access goes through Diesel:\n{}",
        found.join("\n")
    );
}
