use rstest::rstest;

use crate::quality::config::StructureConfig;
use crate::quality::structure::*;
use crate::testutil::write_files;

fn units(files: &[(&str, usize)]) -> Vec<Unit> {
    files
        .iter()
        .map(|(path, production)| Unit {
            path: path.to_string(),
            production: *production,
        })
        .collect()
}

fn rules(files: &[(&str, usize)], cfg: &StructureConfig) -> Vec<(String, String, usize)> {
    analyze(&units(files), cfg)
        .1
        .into_iter()
        .map(|f| (f.path, f.rule, f.value))
        .collect()
}

#[rstest]
#[case::go_test("commit_test.go", "commit")]
#[case::rust_tests("commit_tests.rs", "commit")]
#[case::python_test("test_commit.py", "commit")]
#[case::js_spec("commit.spec.ts", "commit")]
#[case::plain("commit_execute.go", "commit_execute")]
fn pairs_a_test_with_its_code(#[case] file: &str, #[case] want: &str) {
    assert_eq!(stem(file), want);
}

#[test]
fn a_small_grouped_directory_passes() {
    let files = [
        ("pkg/a/one.go", 100),
        ("pkg/a/one_test.go", 50),
        ("pkg/a/two.go", 100),
    ];
    assert!(rules(&files, &StructureConfig::default()).is_empty());
}

#[test]
fn too_many_modules_in_a_directory_fail() {
    let files: Vec<(String, usize)> = (0..13).map(|i| (format!("pkg/m{i}.go"), 10)).collect();
    let files: Vec<(&str, usize)> = files.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    assert_eq!(
        rules(&files, &StructureConfig::default()),
        [("pkg".to_string(), "files".to_string(), 13)]
    );
}

#[test]
fn a_test_does_not_count_as_another_module() {
    let mut files: Vec<(String, usize)> = (0..12).map(|i| (format!("pkg/m{i}.go"), 10)).collect();
    files.extend((0..12).map(|i| (format!("pkg/m{i}_test.go"), 10)));
    let files: Vec<(&str, usize)> = files.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    assert!(rules(&files, &StructureConfig::default()).is_empty());
}

#[test]
fn production_lines_of_the_directory_add_up_without_tests() {
    let cfg = StructureConfig {
        max_dir_production_lines: 250,
        ..Default::default()
    };
    let files = [
        ("pkg/a.go", 100),
        ("pkg/b.go", 100),
        ("pkg/c.go", 100),
        ("pkg/c_test.go", 900),
    ];
    assert_eq!(
        rules(&files, &cfg),
        [("pkg".to_string(), "lines".to_string(), 300)]
    );
}

#[test]
fn modules_sharing_a_first_word_are_a_missing_sub_module() {
    let files = [
        ("pkg/commit_execute.go", 10),
        ("pkg/commit_push.go", 10),
        ("pkg/commit_message.go", 10),
        ("pkg/commit.go", 10),
        ("pkg/commit_test.go", 10),
        ("pkg/other.go", 10),
    ];
    let (_, found) = analyze(&units(&files), &StructureConfig::default());
    assert_eq!(found.len(), 1);
    let f = &found[0];
    assert_eq!((f.rule.as_str(), f.value), ("prefix", 4));
    assert!(f.message.contains("`commit` sub-module"));
    assert!(
        f.files.contains(&"pkg/commit_test.go".to_string()) && !f.files.contains(&"pkg/other.go".to_string())
    );
}

#[test]
fn the_same_word_in_different_directories_is_not_a_group() {
    let files = [("a/x_1.go", 1), ("a/x_2.go", 1), ("b/x_3.go", 1), ("b/x_4.go", 1)];
    assert!(rules(&files, &StructureConfig::default()).is_empty());
}

#[test]
fn files_at_the_root_belong_to_the_dot_directory() {
    let files = [("a_1.rs", 1), ("a_2.rs", 1), ("a_3.rs", 1), ("a_4.rs", 1)];
    assert_eq!(rules(&files, &StructureConfig::default())[0].0, ".");
}

#[test]
fn checks_a_real_tree_and_renders_it() {
    let dir = write_files(&[
        ("src/job_a.rs", "fn a() {}\n"),
        ("src/job_b.rs", "fn b() {}\n"),
        ("src/job_c.rs", "fn c() {}\n"),
        ("src/job_d.rs", "fn d() {}\n"),
        ("src/gen/job_e.rs", "fn e() {}\n"),
        ("notes.md", "# not code\n"),
    ]);
    let cfg = StructureConfig::default();
    let report = check_structure(dir.path(), &cfg).unwrap();
    assert_eq!(
        (report.totals.dirs, report.totals.files, report.totals.failures),
        (2, 5, 1)
    );
    assert!(!report.passed());
    assert!(report.to_text().contains("FAIL  prefix     4 (limit 4)  src:"));
    assert!(report.to_text().contains("2 directories, 5 files: 1 findings"));
    assert!(
        report
            .to_github()
            .starts_with("::error file=src/job_a.rs,title=Structure (prefix)::")
    );
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(json["findings"][0]["rule"], "prefix");

    let excluded = StructureConfig {
        exclude: vec!["src/job_*".into()],
        ..cfg
    };
    assert!(check_structure(dir.path(), &excluded).unwrap().passed());
}

#[rstest]
#[case::missing_file(None, 12, 3000, 4)]
#[case::table(
    Some("[structure]\nmax_files_per_dir = 8\nmax_dir_production_lines = 900\nmax_prefix_group = 3\n"),
    8,
    900,
    3
)]
#[case::partial(Some("[structure]\nmax_prefix_group = 5\n"), 12, 3000, 5)]
fn reads_the_structure_table(
    #[case] file: Option<&str>,
    #[case] files: usize,
    #[case] lines: usize,
    #[case] group: usize,
) {
    let dir = write_files(
        &file
            .map(|f| vec![(crate::quality::config::DIRECTIVES_PATH, f)])
            .unwrap_or_default(),
    );
    let cfg = StructureConfig::load(&dir.path().join(crate::quality::config::DIRECTIVES_PATH)).unwrap();
    assert_eq!(
        (
            cfg.max_files_per_dir,
            cfg.max_dir_production_lines,
            cfg.max_prefix_group
        ),
        (files, lines, group)
    );
}

#[rstest]
#[case::zero_files("[structure]\nmax_files_per_dir = 0\n")]
#[case::zero_lines("[structure]\nmax_dir_production_lines = 0\n")]
#[case::group_of_one("[structure]\nmax_prefix_group = 1\n")]
fn rejects_a_bad_table(#[case] file: &str) {
    let dir = write_files(&[(crate::quality::config::DIRECTIVES_PATH, file)]);
    assert!(StructureConfig::load(&dir.path().join(crate::quality::config::DIRECTIVES_PATH)).is_err());
}
