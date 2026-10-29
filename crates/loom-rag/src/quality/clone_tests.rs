use std::fs;
use std::path::Path;

use rstest::{fixture, rstest};

use crate::quality::config::DupConfig;
use crate::quality::{CloneReport, check_clones};

const ALPHA: &str = "fn alpha(items: &[i32]) -> i32 {\n    let mut total = 0;\n    for item in items {\n        if *item > 10 {\n            total += item * 2;\n        }\n    }\n    total\n}\n";
const BETA: &str = "fn beta(values: &[i32]) -> i32 {\n    let mut sum = 5;\n    for v in values {\n        if *v > 99 {\n            sum += v * 3;\n        }\n    }\n    sum\n}\n";
const DIFFERENT: &str = "fn gamma(items: &[i32]) -> usize {\n    let mut count = 0;\n    for item in items {\n        while *item > 10 {\n            count += 1;\n            break;\n        }\n    }\n    count\n}\n";

#[fixture]
fn small() -> DupConfig {
    DupConfig {
        min_lines: 5,
        min_tokens: 10,
        ..DupConfig::default()
    }
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }
    dir
}

fn run(dir: &Path, cfg: &DupConfig) -> CloneReport {
    check_clones(dir, cfg, &[]).unwrap()
}

#[rstest]
fn finds_blocks_that_differ_only_in_names_and_constants(small: DupConfig) {
    let dir = project(&[("src/a.rs", ALPHA), ("src/b.rs", BETA)]);
    let report = run(dir.path(), &small);
    assert_eq!(report.groups.len(), 1, "{}", report.to_text());
    let places: Vec<(&str, usize, usize)> = report.groups[0]
        .locations
        .iter()
        .map(|l| (l.path.as_str(), l.start_line, l.end_line))
        .collect();
    assert_eq!(places, vec![("src/a.rs", 1, 9), ("src/b.rs", 1, 9)]);
    assert_eq!(report.totals.duplicated_lines, 18);
    assert_eq!(report.totals.lines, 18);
    assert!((report.totals.percent - 100.0).abs() < 1e-9);
    assert!(!report.passed());
}

#[rstest]
fn different_logic_is_not_a_clone(small: DupConfig) {
    let dir = project(&[("src/a.rs", ALPHA), ("src/c.rs", DIFFERENT)]);
    let report = run(dir.path(), &small);
    assert!(report.groups.is_empty(), "{}", report.to_text());
    assert!(report.passed());
    assert_eq!(report.totals.percent, 0.0);
}

#[rstest]
fn finds_copies_inside_one_file_and_ignores_comments(small: DupConfig) {
    let commented = BETA.replace(
        "let mut sum = 5;",
        "// start from five\n    let mut sum = 5; // five",
    );
    let dir = project(&[("src/a.rs", &format!("{ALPHA}\n{commented}"))]);
    let report = run(dir.path(), &small);
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].locations.len(), 2);
    assert!(report.groups[0].locations.iter().all(|l| l.path == "src/a.rs"));
}

#[rstest]
fn reports_the_largest_repeated_block_not_the_blocks_inside_it(small: DupConfig) {
    let dir = project(&[
        ("src/a.rs", ALPHA),
        ("src/b.rs", BETA),
        ("src/c.rs", &ALPHA.replace("alpha", "again")),
    ]);
    let report = run(dir.path(), &small);
    assert_eq!(
        report.groups.len(),
        1,
        "the function, not also its loop and its if:\n{}",
        report.to_text()
    );
    assert_eq!(report.groups[0].locations.len(), 3);
}

#[rstest]
#[case::one_line_short(10, 10, 0)]
#[case::exactly_the_nine_lines(9, 10, 1)]
#[case::too_few_tokens(3, 500, 0)]
fn only_blocks_over_the_thresholds_count(
    #[case] min_lines: usize,
    #[case] min_tokens: usize,
    #[case] groups: usize,
) {
    let dir = project(&[("src/a.rs", ALPHA), ("src/b.rs", BETA)]);
    let report = run(
        dir.path(),
        &DupConfig {
            min_lines,
            min_tokens,
            ..DupConfig::default()
        },
    );
    assert_eq!(report.groups.len(), groups);
}

#[rstest]
fn the_allowance_decides_whether_duplication_fails(small: DupConfig) {
    let dir = project(&[("src/a.rs", ALPHA), ("src/b.rs", BETA), ("src/c.rs", DIFFERENT)]);
    let report = run(
        dir.path(),
        &DupConfig {
            max_percent: 0.0,
            ..small
        },
    );
    assert!(!report.passed());
    let lenient = run(
        dir.path(),
        &DupConfig {
            max_percent: 80.0,
            ..small
        },
    );
    assert!(lenient.passed(), "{:.1}% is within 80%", lenient.totals.percent);
    assert!(lenient.totals.percent > 50.0 && lenient.totals.percent < 80.0);
}

#[fixture]
fn tests_only_copies() -> tempfile::TempDir {
    let tests = format!("#[cfg(test)]\nmod tests {{\n{ALPHA}\n{BETA}}}\n");
    project(&[
        ("src/lib.rs", &format!("fn only() {{}}\n{tests}")),
        (
            "tests/it.rs",
            &format!("{ALPHA}\n{}", ALPHA.replace("alpha", "twin")),
        ),
    ])
}

#[rstest]
fn test_code_is_checked_only_when_asked(tests_only_copies: tempfile::TempDir) {
    let with = run(
        tests_only_copies.path(),
        &DupConfig {
            include_tests: true,
            ..small()
        },
    );
    assert!(
        !with.groups.is_empty(),
        "copies in tests are clones when tests are included"
    );
    let without = run(
        tests_only_copies.path(),
        &DupConfig {
            include_tests: false,
            ..small()
        },
    );
    assert!(without.groups.is_empty(), "{}", without.to_text());
    assert!(without.passed());
}

#[rstest]
fn works_across_languages(small: DupConfig) {
    let py = "def alpha(items):\n    total = 0\n    for item in items:\n        if item > 10:\n            total += item * 2\n    return total\n";
    let go = "package a\n\nfunc Alpha(items []int) int {\n\ttotal := 0\n\tfor _, item := range items {\n\t\tif item > 10 {\n\t\t\ttotal += item * 2\n\t\t}\n\t}\n\treturn total\n}\n";
    let dir = project(&[
        ("a.py", py),
        ("b.py", &py.replace("alpha", "beta").replace("10", "20")),
        ("a.go", go),
        ("b.go", &go.replace("Alpha", "Beta").replace("10", "99")),
    ]);
    let report = run(dir.path(), &small);
    let mut kinds: Vec<&str> = report
        .groups
        .iter()
        .map(|g| g.locations[0].path.rsplit('.').next().unwrap())
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, vec!["go", "py"], "{}", report.to_text());
}

#[rstest]
fn renders_text_json_and_github_annotations(small: DupConfig) {
    let dir = project(&[("src/a.rs", ALPHA), ("src/b.rs", BETA)]);
    let report = run(dir.path(), &small);
    let text = report.to_text();
    assert!(
        text.contains("clone #1") && text.contains("src/a.rs:1-9") && text.contains("src/b.rs:1-9"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("in 1 groups"), "{text}");
    let github = report.to_github();
    assert!(github.contains("::error file=src/a.rs,line=1,endLine=9,title=Duplicated code::these lines are repeated, also at src/b.rs:1-9"), "{github}");
    assert!(github.contains("::error file=src/b.rs,line=1,endLine=9,title=Duplicated code::these lines are repeated, also at src/a.rs:1-9"), "{github}");
    let lenient = run(
        dir.path(),
        &DupConfig {
            max_percent: 100.0,
            ..small
        },
    );
    assert!(
        lenient.to_github().starts_with("::warning "),
        "within the allowance the annotations only warn"
    );
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(json["schema"], 1);
    assert_eq!(json["totals"]["groups"], 1);
    assert_eq!(json["groups"][0]["locations"][1]["path"], "src/b.rs");
}

#[rstest]
fn skips_build_output_and_honours_excludes(small: DupConfig) {
    let dir = project(&[
        ("src/a.rs", ALPHA),
        ("target/b.rs", BETA),
        ("vendor_copy/c.rs", BETA),
    ]);
    assert!(
        run(dir.path(), &small).groups.len() == 1,
        "target/ is never read, vendor_copy/ is"
    );
    let excluded = check_clones(dir.path(), &small, &["vendor_copy/**".to_string()]).unwrap();
    assert!(excluded.groups.is_empty());
}

#[rstest]
fn an_empty_project_has_nothing_duplicated(small: DupConfig) {
    let report = run(project(&[("README.md", "# nothing\n")]).path(), &small);
    assert_eq!(
        (report.totals.files, report.totals.lines, report.totals.percent),
        (0, 0, 0.0)
    );
    assert!(report.passed());
}
