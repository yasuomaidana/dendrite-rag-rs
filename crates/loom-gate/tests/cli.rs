//! The `loom-gate` binary end to end: exit codes and output formats.

use rstest::{fixture, rstest};
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
#[fixture]
fn project(#[default(501)] lines: usize) -> tempfile::TempDir {
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

#[rstest]
fn passes_at_the_limit_and_fails_one_line_over_whatever_the_tests_add(
    #[from(project)] over_project: tempfile::TempDir,
) {
    let ok = run_loc(project(500).path(), &[]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stdout)
    );
    let over = run_loc(over_project.path(), &[]);
    assert_eq!(over.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&over.stdout).contains("FAIL    501 production lines  src/lib.rs"));
}

#[rstest]
fn json_and_github_formats(#[from(project)] dir: tempfile::TempDir) {
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

const ALPHA: &str = "fn alpha(items: &[i32]) -> i32 {\n    let mut total = 0;\n    for item in items {\n        if *item > 10 {\n            total += item * 2;\n        }\n    }\n    total\n}\n";
const BETA: &str = "fn beta(values: &[i32]) -> i32 {\n    let mut sum = 5;\n    for v in values {\n        if *v > 99 {\n            sum += v * 3;\n        }\n    }\n    sum\n}\n";

fn run_cmd(command: &str, root: &Path, extra: &[&str]) -> Output {
    let mut args = vec![command, "--root", root.to_str().unwrap()];
    args.extend_from_slice(extra);
    gate(&args)
}

#[fixture]
fn cloned_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.rs", ALPHA);
    write(dir.path(), "src/b.rs", BETA);
    dir
}

#[rstest]
fn clones_fail_over_the_allowance_and_pass_within_it(#[from(cloned_project)] dir: tempfile::TempDir) {
    let out = run_cmd("clones", dir.path(), &["--min-tokens", "10"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("clone #1") && text.contains("src/a.rs:1-9") && text.contains("src/b.rs:1-9"),
        "{text}"
    );
    assert_eq!(
        run_cmd(
            "clones",
            dir.path(),
            &["--min-tokens", "10", "--max-percent", "100"]
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        run_cmd("clones", dir.path(), &["--min-tokens", "10", "--min-lines", "20"])
            .status
            .code(),
        Some(0),
        "nothing that long repeats"
    );
}

#[rstest]
fn clones_read_the_directives_and_print_json_and_annotations(#[from(cloned_project)] dir: tempfile::TempDir) {
    write(
        dir.path(),
        ".github/dendrite/directives.toml",
        "[duplication]\nmax_percent = 0.0\nmin_tokens = 10\nmin_lines = 5\n",
    );
    let json = run_cmd("clones", dir.path(), &["--format", "json"]);
    assert_eq!(
        json.status.code(),
        Some(1),
        "the directives file is read by default"
    );
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        (v["schema"].clone(), v["totals"]["groups"].clone()),
        (1.into(), 1.into())
    );
    let github = run_cmd("clones", dir.path(), &["--format", "github"]);
    assert!(String::from_utf8_lossy(&github.stdout).starts_with("::error file=src/a.rs,line=1,endLine=9,"));
    write(
        dir.path(),
        ".github/dendrite/directives.toml",
        "[duplication]\nmax_percent = 100.0\nmin_tokens = 10\n",
    );
    assert_eq!(run_cmd("clones", dir.path(), &[]).status.code(), Some(0));
    write(
        dir.path(),
        ".github/dendrite/directives.toml",
        "[duplication]\nmin_tokens = 1\n",
    );
    assert_eq!(
        run_cmd("clones", dir.path(), &[]).status.code(),
        Some(2),
        "invalid limits are a tool error"
    );
}

#[rstest]
fn clones_can_leave_out_tests_and_files(#[from(cloned_project)] src: tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "tests/a.rs", ALPHA);
    write(dir.path(), "tests/b.rs", BETA);
    assert_eq!(
        run_cmd("clones", dir.path(), &["--min-tokens", "10"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        run_cmd(
            "clones",
            dir.path(),
            &["--min-tokens", "10", "--include-tests", "false"]
        )
        .status
        .code(),
        Some(0)
    );
    assert_eq!(
        run_cmd(
            "clones",
            src.path(),
            &["--min-tokens", "10", "--exclude", "src/b.rs"]
        )
        .status
        .code(),
        Some(0)
    );
}

#[test]
fn rstest_fails_on_repeated_tests_and_passes_on_clean_code() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/lib.rs",
        "fn add(a: i32, b: i32) -> i32 { a + b }\n",
    );
    let ok = run_cmd("rstest", dir.path(), &[]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stdout)
    );
    let tests = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn small() {\n        let left = 1;\n        let right = 2;\n        assert_eq!(add(left, right), 3);\n    }\n    #[test]\n    fn large() {\n        let left = 100;\n        let right = 200;\n        assert_eq!(add(left, right), 300);\n    }\n}\n";
    write(
        dir.path(),
        "src/lib.rs",
        &format!("fn add(a: i32, b: i32) -> i32 {{ a + b }}\n{tests}"),
    );
    let bad = run_cmd("rstest", dir.path(), &[]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("[repeated_test]"));
    let json = run_cmd("rstest", dir.path(), &["--format", "json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["violations"][0]["kind"], "repeated_test");
    assert!(
        String::from_utf8_lossy(&run_cmd("rstest", dir.path(), &["--format", "github"]).stdout)
            .starts_with("::error file=src/lib.rs,")
    );
    assert_eq!(
        run_cmd("rstest", dir.path(), &["--exclude", "src/**"])
            .status
            .code(),
        Some(0)
    );
}
