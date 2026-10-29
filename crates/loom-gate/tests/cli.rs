//! The `loom-gate` binary end to end: exit codes and output formats.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn gate(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loom-gate"))
        .args(args)
        .output()
        .unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn production(n: usize) -> String {
    (0..n).map(|i| format!("fn f{i}() {{}}\n")).collect()
}

/// A tree whose only file has `lines` production lines plus 300 lines of tests.
fn project(lines: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let tests: String = (0..300).map(|i| format!("    fn t{i}() {{}}\n")).collect();
    write(
        dir.path(),
        "src/lib.rs",
        &format!("{}#[cfg(test)]\nmod tests {{\n{tests}}}\n", production(lines)),
    );
    dir
}

fn run_loc(root: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["loc", "--root", root.to_str().unwrap()];
    args.extend_from_slice(extra);
    gate(&args)
}

#[test]
fn passes_at_the_limit_and_fails_one_line_over_whatever_the_tests_add() {
    let ok = run_loc(project(500).path(), &[]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stdout)
    );
    let over = run_loc(project(501).path(), &[]);
    assert_eq!(over.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&over.stdout).contains("FAIL    501 production lines  src/lib.rs"));
}

#[test]
fn json_and_github_formats() {
    let dir = project(501);
    let json = run_loc(dir.path(), &["--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        (v["schema"].clone(), v["totals"]["failures"].clone()),
        (1.into(), 1.into())
    );
    assert_eq!(v["files"][0]["production"], 501);
    let github = run_loc(dir.path(), &["--format", "github"]);
    assert!(String::from_utf8_lossy(&github.stdout).starts_with("::error file=src/lib.rs,line=1,"));
}

#[test]
fn flags_and_directives_set_the_limits() {
    let dir = project(120);
    assert_eq!(
        run_loc(dir.path(), &["--max", "100", "--warn-at", "90"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        run_loc(
            dir.path(),
            &["--exclude", "src/**", "--max", "100", "--warn-at", "90"]
        )
        .status
        .code(),
        Some(0)
    );
    write(
        dir.path(),
        ".github/dendrite/directives.toml",
        "[loc]\nmax_production_lines = 100\nwarn_at = 80\n",
    );
    assert_eq!(
        run_loc(dir.path(), &[]).status.code(),
        Some(1),
        "the directives file is read by default"
    );
    let other = dir.path().join("elsewhere.toml");
    fs::write(&other, "[loc]\nmax_production_lines = 200\nwarn_at = 150\n").unwrap();
    assert_eq!(
        run_loc(dir.path(), &["--config", other.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn a_tool_error_is_exit_two() {
    let dir = project(1);
    write(dir.path(), ".github/dendrite/directives.toml", "[loc\n");
    let bad = run_loc(dir.path(), &[]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not a valid directives file"));
    assert_eq!(
        gate(&["loc", "--bogus"]).status.code(),
        Some(2),
        "a usage error is exit 2 as well"
    );
    assert_eq!(
        run_loc(
            dir.path(),
            &["--max", "10", "--warn-at", "20", "--config", "/nonexistent.toml"]
        )
        .status
        .code(),
        Some(2)
    );
}
