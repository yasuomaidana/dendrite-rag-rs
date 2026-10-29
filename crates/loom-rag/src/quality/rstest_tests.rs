use std::fs;

use rstest::rstest;

use crate::quality::check_rstest;
use crate::quality::rstest_check::check_source;

const TWO_SAME_TESTS: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds_small() {\n        let left = 1;\n        let right = 2;\n        assert_eq!(add(left, right), 3);\n    }\n\n    #[test]\n    fn adds_large() {\n        let left = 100;\n        let right = 200;\n        assert_eq!(add(left, right), 300);\n    }\n}\n";

fn kinds(src: &str) -> Vec<String> {
    check_source("src/lib.rs", src)
        .into_iter()
        .map(|v| v.kind)
        .collect()
}

#[test]
fn tests_that_differ_only_in_constants_should_be_cases() {
    let found = check_source("src/lib.rs", TWO_SAME_TESTS);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "repeated_test");
    assert_eq!(found[0].line, 4);
    assert!(
        found[0].message.contains("adds_small")
            && found[0].message.contains("adds_large")
            && found[0].message.contains("#[case]")
    );
}

#[rstest]
#[case::different_structure(
    "    #[test]\n    fn first() {\n        let left = 1;\n        let right = 2;\n        assert_eq!(add(left, right), 3);\n    }\n    #[test]\n    fn second() {\n        let items = vec![1, 2, 3];\n        let total: i32 = items.iter().sum();\n        assert!(total > 0 && total < 10);\n    }\n"
)]
#[case::already_parameterised(
    "    #[rstest]\n    #[case(1, 2)]\n    fn first(#[case] left: i32, #[case] right: i32) {\n        let sum = add(left, right);\n        assert!(sum > 0 && sum < 99);\n    }\n    #[rstest]\n    #[case(5, 6)]\n    fn second(#[case] left: i32, #[case] right: i32) {\n        let sum = add(left, right);\n        assert!(sum > 0 && sum < 99);\n    }\n"
)]
#[case::too_small_to_matter(
    "    #[test]\n    fn first() { assert_eq!(add(1, 2), 3); }\n    #[test]\n    fn second() { assert_eq!(add(4, 5), 9); }\n"
)]
fn leaves_fine_tests_alone(#[case] body: &str) {
    assert!(kinds(&format!("mod tests {{\n{body}}}\n")).is_empty());
}

const HELPER_TWICE: &str = "mod tests {\n    fn open() -> Store {\n        Store::new()\n    }\n    #[test]\n    fn writes() {\n        let store = open();\n        store.put(1);\n    }\n    #[test]\n    fn reads() {\n        let store = open().with_data();\n        store.get(1);\n    }\n}\n";

#[test]
fn a_setup_helper_two_tests_bind_should_be_a_fixture() {
    let found = check_source("src/lib.rs", HELPER_TWICE);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, "helper_should_be_fixture");
    assert_eq!(found[0].line, 2);
    assert!(
        found[0].message.contains("open()")
            && found[0].message.contains("writes")
            && found[0].message.contains("reads")
    );
}

#[rstest]
#[case::used_by_one_test(
    "mod tests {\n    fn open() -> Store { Store::new() }\n    #[test]\n    fn writes() {\n        let store = open();\n        store.put(1);\n    }\n    #[test]\n    fn other() {\n        assert!(true);\n    }\n}\n"
)]
#[case::already_a_fixture(
    "mod tests {\n    #[fixture]\n    fn open() -> Store { Store::new() }\n    #[rstest]\n    fn writes(open: Store) {\n        open.put(1);\n    }\n}\n"
)]
#[case::called_without_binding(
    "mod tests {\n    fn reset() { clear(); }\n    #[test]\n    fn first() {\n        reset();\n        check(1);\n    }\n    #[test]\n    fn second() {\n        reset();\n        check(2);\n    }\n}\n"
)]
fn leaves_fine_helpers_alone(#[case] src: &str) {
    assert!(kinds(src).is_empty(), "{:?}", kinds(src));
}

#[test]
fn nested_modules_are_checked_on_their_own() {
    let src = format!(
        "mod outer {{\n{TWO_SAME_TESTS}\n    mod inner {{\n        fn open() -> Store {{ Store::new() }}\n        #[test]\n        fn only() {{\n            let s = open();\n            s.put(1);\n        }}\n    }}\n}}\n"
    );
    let k = kinds(&src);
    assert_eq!(
        k,
        vec!["repeated_test".to_string()],
        "one test in the inner module is no violation"
    );
}

#[test]
fn a_directory_check_counts_rust_files_and_orders_the_findings() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/b.rs"), TWO_SAME_TESTS).unwrap();
    fs::write(dir.path().join("src/a.rs"), HELPER_TWICE).unwrap();
    fs::write(dir.path().join("src/clean.rs"), "fn main() {}\n").unwrap();
    fs::write(dir.path().join("notes.md"), "# not rust\n").unwrap();
    let report = check_rstest(dir.path(), &[]).unwrap();
    assert_eq!(report.files, 3);
    let order: Vec<&str> = report.violations.iter().map(|v| v.path.as_str()).collect();
    assert_eq!(order, vec!["src/a.rs", "src/b.rs"]);
    assert!(!report.passed());
    let text = report.to_text();
    assert!(
        text.contains("src/a.rs:2  [helper_should_be_fixture]")
            && text.trim_end().ends_with("3 Rust files checked, 2 violations"),
        "{text}"
    );
    assert!(
        report
            .to_github()
            .contains("::error file=src/b.rs,line=4,title=Use rstest::adds_small, adds_large differ")
    );
    let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
    assert_eq!(
        (json["schema"].clone(), json["files"].clone()),
        (1.into(), 3.into())
    );

    let excluded = check_rstest(dir.path(), &["src/*.rs".to_string()]).unwrap();
    assert!(excluded.passed() && excluded.files == 0);
}

#[rstest]
#[case::same_arguments_are_setup(
    "mod tests {\n    fn open(dir: &Path) -> Store {\n        Store::new(dir)\n    }\n    #[test]\n    fn writes() {\n        let store = open(&a);\n        store.put(1);\n    }\n    #[test]\n    fn reads() {\n        let store = open(&a);\n        store.get(1);\n    }\n}\n",
    1
)]
#[case::different_arguments_are_a_builder(
    "mod tests {\n    fn open(dir: &Path) -> Store {\n        Store::new(dir)\n    }\n    #[test]\n    fn writes() {\n        let store = open(&a);\n        store.put(1);\n    }\n    #[test]\n    fn reads() {\n        let store = open(&b);\n        store.get(1);\n    }\n}\n",
    0
)]
fn a_helper_with_parameters_is_setup_only_when_the_calls_repeat(#[case] src: &str, #[case] expected: usize) {
    assert_eq!(kinds(src).len(), expected);
}

#[rstest]
#[case::argument_built_by_the_test(
    "mod tests {\n    fn open(dir: &Path) -> Store {\n        Store::new(dir)\n    }\n    #[test]\n    fn writes() {\n        let dir = make();\n        let store = open(dir.path());\n        store.put(1);\n    }\n    #[test]\n    fn reads() {\n        let dir = make();\n        let store = open(dir.path());\n        store.get(1);\n    }\n}\n"
)]
fn a_call_that_uses_what_the_test_built_is_a_builder(#[case] src: &str) {
    assert!(kinds(src).is_empty());
}
