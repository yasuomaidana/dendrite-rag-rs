use std::fs;
use std::path::Path;

use rstest::{fixture, rstest};

use super::*;
use crate::lang::Lang;
use crate::quality::config::DIRECTIVES_PATH;

fn counts(path: &str, lang: Lang, src: &str) -> (usize, usize, usize, usize) {
    let c = count_lines(path, lang, src);
    (c.production, c.test, c.comment, c.blank)
}

const RUST_INLINE_TESTS: &str = "//! crate doc\n\n/// doc\nfn main() {\n    let x = 1; // trailing\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn t() {\n        assert!(true);\n    }\n}\n";
const PYTHON: &str = "\"\"\"Module doc.\"\"\"\n\nimport os\n\n\ndef real():\n    \"\"\"Doc.\"\"\"\n    return 1\n\n\ndef test_real():\n    assert real() == 1\n\n\nclass TestAll:\n    def test_x(self):\n        pass\n\n\nif __name__ == \"__main__\":\n    real()\n";
const TS_SUITE: &str = "// c\nexport const a = 1;\n\ndescribe('x', () => {\n  it('y', () => {\n    expect(a).toBe(1);\n  });\n});\n";
const JAVA: &str = "public class A {\n    // c\n    int f() { return 1; }\n\n    @Test\n    void t() {\n        assert true;\n    }\n}\n";

// (path, language, source, production, test, comment, blank)
#[rstest]
#[case::rust_inline_test_module("src/lib.rs", Lang::Rust, RUST_INLINE_TESTS, 3, 8, 2, 3)]
#[case::rust_lone_test_fn(
    "src/lib.rs",
    Lang::Rust,
    "#[test]\nfn lone() {\n    assert!(true);\n}\nfn prod() {}\n",
    1,
    4,
    0,
    0
)]
#[case::rust_cfg_not_test_is_production(
    "src/lib.rs",
    Lang::Rust,
    "#[cfg(not(test))]\nfn real() {}\n",
    2,
    0,
    0,
    0
)]
#[case::rust_cfg_any_test(
    "src/lib.rs",
    Lang::Rust,
    "#[cfg(any(test, feature = \"x\"))]\nfn helper() {}\n",
    0,
    2,
    0,
    0
)]
#[case::rust_rstest_with_cases(
    "src/lib.rs",
    Lang::Rust,
    "#[rstest]\n#[case(1)]\nfn t(#[case] a: i32) {}\n",
    0,
    3,
    0,
    0
)]
#[case::rust_tokio_test("src/lib.rs", Lang::Rust, "#[tokio::test]\nasync fn t() {}\n", 0, 2, 0, 0)]
#[case::rust_inner_cfg_test_marks_the_file(
    "src/helpers.rs",
    Lang::Rust,
    "#![cfg(test)]\nfn x() {}\n",
    0,
    2,
    0,
    0
)]
#[case::rust_block_comment("src/lib.rs", Lang::Rust, "/* a\n b */\nfn f() {}\n", 1, 0, 2, 0)]
#[case::rust_test_directory("tests/it.rs", Lang::Rust, "fn t() {}\n\nfn u() {}\n", 0, 2, 0, 1)]
#[case::python_tests_docstrings_and_main_guard("app/a.py", Lang::Python, PYTHON, 3, 7, 2, 9)]
#[case::python_test_file("app/test_a.py", Lang::Python, "def f():\n    pass\n", 0, 2, 0, 0)]
#[case::go_comment(
    "main.go",
    Lang::Go,
    "package main\n\n// Doc.\nfunc main() {\n\tprintln(1)\n}\n",
    4,
    0,
    1,
    1
)]
#[case::go_test_file(
    "main_test.go",
    Lang::Go,
    "package main\n\nfunc TestX(t *testing.T) {}\n",
    0,
    2,
    0,
    1
)]
#[case::typescript_suite("src/a.ts", Lang::TypeScript, TS_SUITE, 1, 5, 1, 1)]
#[case::typescript_test_file("src/a.test.ts", Lang::TypeScript, TS_SUITE, 0, 7, 0, 1)]
#[case::java_test_method("A.java", Lang::Java, JAVA, 3, 4, 1, 1)]
#[case::java_test_class("ATest.java", Lang::Java, "class ATest {\n    void f() {}\n}\n", 0, 3, 0, 0)]
#[case::c_comment("main.c", Lang::C, "/* c */\nint main(void) {\n  return 0;\n}\n", 3, 0, 1, 0)]
#[case::c_test_file("test_foo.c", Lang::C, "int t(void) { return 0; }\n", 0, 1, 0, 0)]
#[case::shell_counts_non_blank_lines("run.sh", Lang::Text, "#!/bin/sh\n\necho hi\n", 2, 0, 0, 1)]
#[case::empty_file("empty.rs", Lang::Rust, "", 0, 0, 0, 0)]
#[case::no_trailing_newline("a.rs", Lang::Rust, "fn a() {}", 1, 0, 0, 0)]
fn counts_production_test_comment_and_blank_lines(
    #[case] path: &str,
    #[case] lang: Lang,
    #[case] src: &str,
    #[case] production: usize,
    #[case] test: usize,
    #[case] comment: usize,
    #[case] blank: usize,
) {
    assert_eq!(counts(path, lang, src), (production, test, comment, blank));
}

#[test]
fn a_syntax_error_is_reported_and_the_rest_still_counts() {
    let c = count_lines("src/lib.rs", Lang::Rust, "fn broken( {\n");
    assert!(c.parse_errors);
    assert_eq!(c.production, 1);
    assert!(!count_lines("src/lib.rs", Lang::Rust, "fn ok() {}\n").parse_errors);
}

#[rstest]
#[case("tests/a.rs", true)]
#[case("src/tests/a.rs", true)]
#[case("web/__tests__/a.ts", true)]
#[case("src/a_test.go", true)]
#[case("web/a.test.ts", true)]
#[case("web/a.spec.tsx", true)]
#[case("py/test_a.py", true)]
#[case("py/conftest.py", true)]
#[case("java/ATest.java", true)]
#[case("src/main.rs", false)]
#[case("src/attest.rs", false)]
#[case("py/latest.py", false)]
#[case("src/contest/a.rs", false)]
fn recognises_test_files_by_name_and_directory(#[case] path: &str, #[case] expected: bool) {
    assert_eq!(is_test_path(path), expected);
}

#[rstest]
#[case(0, Status::Ok)]
#[case(450, Status::Ok)]
#[case(451, Status::Warn)]
#[case(500, Status::Warn)]
#[case(501, Status::Fail)]
fn judges_production_lines_against_the_limits(#[case] production: usize, #[case] expected: Status) {
    let c = LocCount {
        production,
        ..LocCount::default()
    };
    assert_eq!(Status::of(&c, &LocConfig::default()), expected);
}

fn rust_file(production: usize, test: usize) -> String {
    let mut s: String = (0..production).map(|i| format!("fn f{i}() {{}}\n")).collect();
    if test > 0 {
        s.push_str("#[cfg(test)]\nmod tests {\n");
        s.extend((0..test.saturating_sub(3)).map(|i| format!("    fn t{i}() {{}}\n")));
        s.push_str("}\n");
    }
    s
}

#[fixture]
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, body: String| {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    };
    write("src/big.rs", rust_file(501, 300));
    write("src/edge.rs", rust_file(500, 300));
    write("src/near.rs", rust_file(451, 0));
    write("src/small.rs", rust_file(10, 0));
    write("gen/generated.rs", rust_file(900, 0));
    write("target/debug/x.rs", rust_file(900, 0));
    write("README.md", "# docs\n".repeat(900));
    dir
}

#[rstest]
fn finds_only_the_files_over_the_limits(project: tempfile::TempDir) {
    let cfg = LocConfig {
        exclude: vec!["gen/**".into()],
        ..LocConfig::default()
    };
    let report = check_loc(project.path(), &cfg).unwrap();
    let status = |p: &str| {
        report
            .files
            .iter()
            .find(|f| f.path == p)
            .map(|f| (f.production, f.status))
    };
    assert_eq!(status("src/big.rs"), Some((501, Status::Fail)));
    assert_eq!(status("src/edge.rs"), Some((500, Status::Warn)));
    assert_eq!(status("src/near.rs"), Some((451, Status::Warn)));
    assert_eq!(status("src/small.rs"), Some((10, Status::Ok)));
    assert_eq!(status("gen/generated.rs"), None, "excluded by glob");
    assert_eq!(status("target/debug/x.rs"), None, "build output is never counted");
    assert_eq!(status("README.md"), None, "prose is not code");
    assert!(!report.passed());
    assert_eq!(
        (
            report.totals.failures,
            report.totals.warnings,
            report.totals.files
        ),
        (1, 2, 4)
    );
}

#[rstest]
fn a_file_of_mostly_tests_passes(project: tempfile::TempDir) {
    let cfg = LocConfig {
        exclude: vec!["gen/**".into(), "src/big.rs".into()],
        ..LocConfig::default()
    };
    let report = check_loc(project.path(), &cfg).unwrap();
    assert!(
        report.passed(),
        "src/edge.rs has 800 raw lines but 500 production lines"
    );
    let edge = report.files.iter().find(|f| f.path == "src/edge.rs").unwrap();
    assert!(edge.test >= 300 && edge.production == 500);
}

#[rstest]
fn renders_text_json_and_github_annotations(project: tempfile::TempDir) {
    let report = check_loc(
        project.path(),
        &LocConfig {
            exclude: vec!["gen/**".into()],
            ..LocConfig::default()
        },
    )
    .unwrap();
    let text = report.to_text();
    assert!(
        text.starts_with("FAIL    501 production lines  src/big.rs"),
        "{text}"
    );
    assert!(text.contains("warn    500 production lines  src/edge.rs"));
    assert!(text.trim_end().ends_with("1 over 500, 2 over 450"), "{text}");
    let github = report.to_github();
    assert!(
        github.contains(
            "::error file=src/big.rs,line=1,title=Production lines::501 production lines, over 500"
        )
    );
    assert!(github.contains(
        "::warning file=src/near.rs,line=1,title=Production lines::451 production lines, over 450"
    ));
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(json["schema"], 1);
    assert_eq!(json["totals"]["failures"], 1);
    assert_eq!(json["files"][0]["path"], "src/big.rs");
    assert_eq!(json["files"][0]["status"], "fail");
}

#[test]
fn config_defaults_overrides_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(DIRECTIVES_PATH);
    assert_eq!(
        LocConfig::load(&path).unwrap(),
        LocConfig::default(),
        "a missing file means the defaults"
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();

    fs::write(&path, "schema = 1\n[coverage]\nmin_percent = 90.0\n").unwrap();
    assert_eq!(
        LocConfig::load(&path).unwrap(),
        LocConfig::default(),
        "no [loc] table means the defaults"
    );

    fs::write(
        &path,
        "[loc]\nmax_production_lines = 300\nwarn_at = 250\nexclude = [\"a/**\"]\n",
    )
    .unwrap();
    let cfg = LocConfig::load(&path).unwrap();
    assert_eq!(
        (cfg.max_production_lines, cfg.warn_at, cfg.exclude),
        (300, 250, vec!["a/**".to_string()])
    );

    fs::write(&path, "[loc]\nmax_production_lines = 300\n").unwrap();
    assert!(
        LocConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("warn_at (450) must not exceed")
    );
    fs::write(&path, "[loc]\nmax_production_lines = 0\n").unwrap();
    assert!(LocConfig::load(&path).is_err());
    fs::write(&path, "[loc\n").unwrap();
    assert!(
        LocConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("not a valid directives file")
    );
    assert!(
        LocConfig::load(dir.path()).is_err(),
        "a directory is not a readable file"
    );
}

#[test]
fn reads_the_shared_directives_example() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/specs/directives.example.toml");
    let cfg = LocConfig::load(&example).unwrap();
    assert_eq!(
        cfg,
        LocConfig {
            max_production_lines: 500,
            warn_at: 450,
            exclude: vec!["**/generated/**".into()]
        }
    );
}
