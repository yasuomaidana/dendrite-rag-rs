use rstest::rstest;

use super::*;
use crate::quality::config::{ComplexityConfig, DIRECTIVES_PATH};
use crate::testutil::write_files;

const SIMPLE: &str = "fn plain() {}\n";
const BUSY: &str = "fn busy(a: bool, b: bool) {\n    if a { if b { if a { if b {} } } }\n}\n";

#[rstest]
#[case::within_the_limits(ComplexityConfig::default(), 0)]
#[case::cognitive_over(ComplexityConfig { max_cognitive: 5, ..Default::default() }, 1)]
#[case::cyclomatic_over(ComplexityConfig { max_cyclomatic: 3, ..Default::default() }, 1)]
fn judges_functions_against_the_limits(#[case] cfg: ComplexityConfig, #[case] failures: usize) {
    let dir = write_files(&[("src/lib.rs", SIMPLE), ("src/busy.rs", BUSY)]);
    let report = check_complexity(dir.path(), &cfg).unwrap();
    assert_eq!((report.totals.files, report.totals.functions), (2, 2));
    assert_eq!(report.totals.failures, failures);
    assert_eq!(report.passed(), failures == 0);
}

#[test]
fn reports_where_and_how_bad() {
    let dir = write_files(&[("src/busy.rs", BUSY)]);
    let report = check_complexity(
        dir.path(),
        &ComplexityConfig {
            max_cyclomatic: 3,
            ..Default::default()
        },
    )
    .unwrap();
    let f = &report.findings[0];
    assert_eq!(
        (f.path.as_str(), f.symbol.as_str(), f.language.as_str()),
        ("src/busy.rs", "busy", "rust")
    );
    assert_eq!(
        (f.start_line, f.end_line, f.cyclomatic, f.cognitive),
        (1, 3, 5, 10)
    );
}

#[test]
fn judges_test_code_like_any_other() {
    let dir = write_files(&[("tests/it.rs", BUSY)]);
    assert_eq!(
        check_complexity(
            dir.path(),
            &ComplexityConfig {
                max_cyclomatic: 3,
                ..Default::default()
            }
        )
        .unwrap()
        .totals
        .failures,
        1
    );
}

#[test]
fn skips_files_without_a_grammar_and_excluded_ones() {
    let dir = write_files(&[("notes.md", BUSY), ("src/busy.rs", BUSY), ("gen/busy.rs", BUSY)]);
    let cfg = ComplexityConfig {
        exclude: vec!["gen/**".into()],
        ..ComplexityConfig {
            max_cyclomatic: 3,
            ..Default::default()
        }
    };
    let report = check_complexity(dir.path(), &cfg).unwrap();
    assert_eq!((report.totals.files, report.totals.failures), (1, 1));
}

#[test]
fn lists_the_worst_function_first() {
    let dir = write_files(&[
        ("a.rs", BUSY),
        ("b.go", "package p\nfunc f(a bool) { if a {} }\n"),
    ]);
    let report = check_complexity(
        dir.path(),
        &ComplexityConfig {
            max_cyclomatic: 1,
            max_cognitive: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let order: Vec<_> = report.findings.iter().map(|f| f.symbol.as_str()).collect();
    assert_eq!(order, ["busy", "f"]);
}

#[test]
fn renders_json_text_and_github() {
    let dir = write_files(&[("src/busy.rs", BUSY)]);
    let report = check_complexity(
        dir.path(),
        &ComplexityConfig {
            max_cyclomatic: 3,
            ..Default::default()
        },
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(
        (json["schema"].clone(), json["totals"]["failures"].clone()),
        (1.into(), 1.into())
    );
    assert!(
        report
            .to_text()
            .contains("FAIL  cyclomatic   5  cognitive  10  src/busy.rs:1 busy")
    );
    assert!(
        report
            .to_text()
            .contains("1 files, 1 functions: 1 over cyclomatic 3 or cognitive 15")
    );
    assert!(
        report
            .to_github()
            .starts_with("::error file=src/busy.rs,line=1,endLine=3,title=Complexity::busy")
    );
}

#[rstest]
#[case::missing_file(None, 10, 15)]
#[case::no_table(Some("[loc]\nmax_production_lines = 300\n"), 10, 15)]
#[case::table(Some("[complexity]\nmax_cyclomatic = 7\nmax_cognitive = 9\n"), 7, 9)]
#[case::partial(Some("[complexity]\nmax_cognitive = 4\n"), 10, 4)]
fn reads_the_complexity_table(#[case] file: Option<&str>, #[case] cyclomatic: u32, #[case] cognitive: u32) {
    let dir = write_files(&file.map(|f| vec![(DIRECTIVES_PATH, f)]).unwrap_or_default());
    let cfg = ComplexityConfig::load(&dir.path().join(DIRECTIVES_PATH)).unwrap();
    assert_eq!((cfg.max_cyclomatic, cfg.max_cognitive), (cyclomatic, cognitive));
}

#[rstest]
#[case::zero_cyclomatic("[complexity]\nmax_cyclomatic = 0\n")]
#[case::zero_cognitive("[complexity]\nmax_cognitive = 0\n")]
#[case::not_toml("[complexity\n")]
fn rejects_a_bad_table(#[case] file: &str) {
    let dir = write_files(&[(DIRECTIVES_PATH, file)]);
    assert!(ComplexityConfig::load(&dir.path().join(DIRECTIVES_PATH)).is_err());
}
