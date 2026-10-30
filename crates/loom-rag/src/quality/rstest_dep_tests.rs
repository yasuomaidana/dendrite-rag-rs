use std::fs;
use std::path::Path;

use rstest::{fixture, rstest};
use tempfile::TempDir;

use crate::quality::check_rstest;
use crate::quality::rstest_dep::check_manifest;

const PLAIN_TEST: &str = "#[test]\nfn works() {\n    assert_eq!(1 + 1, 2);\n}\n";

#[fixture]
fn krate() -> TempDir {
    let dir = TempDir::new().expect("temp dir");
    fs::create_dir_all(dir.path().join("src")).expect("src dir");
    fs::write(dir.path().join("src/lib.rs"), PLAIN_TEST).expect("lib.rs");
    dir
}

fn manifest(dir: &Path, text: &str) {
    fs::write(dir.join("Cargo.toml"), text).expect("manifest");
}

fn kind_of(dir: &Path) -> Option<String> {
    check_manifest(dir, &dir.join("src/lib.rs")).map(|v| v.kind)
}

#[rstest]
#[case::missing("[package]\nname = \"a\"\n", Some("missing_rstest"))]
#[case::in_other_table_only(
    "[package]\nname = \"a\"\n[dependencies]\nserde = \"1\"\n",
    Some("missing_rstest")
)]
#[case::old_string(
    "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = \"0.26.1\"\n",
    Some("outdated_rstest")
)]
#[case::old_caret(
    "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = \"^0.27\"\n",
    Some("outdated_rstest")
)]
#[case::old_table(
    "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = { version = \"=0.18.2\", default-features = false }\n",
    Some("outdated_rstest")
)]
#[case::current("[package]\nname = \"a\"\n[dev-dependencies]\nrstest = \"0.28.0\"\n", None)]
#[case::newer(
    "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = \">=0.29, <1\"\n",
    None
)]
#[case::path_dependency(
    "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = { path = \"../rstest\" }\n",
    None
)]
fn the_manifest_decides(krate: TempDir, #[case] toml: &str, #[case] expected: Option<&str>) {
    manifest(krate.path(), toml);
    assert_eq!(kind_of(krate.path()).as_deref(), expected);
}

#[rstest]
#[case::current("0.28.0", None)]
#[case::outdated("0.20.0", Some("outdated_rstest"))]
fn a_workspace_member_inherits_the_workspace_version(
    krate: TempDir,
    #[case] workspace_version: &str,
    #[case] expected: Option<&str>,
) {
    let member = krate.path().join("crates/a");
    fs::create_dir_all(member.join("src")).expect("member");
    fs::write(member.join("src/lib.rs"), PLAIN_TEST).expect("lib.rs");
    manifest(
        krate.path(),
        &format!(
            "[workspace]\nmembers = [\"crates/a\"]\n[workspace.dependencies]\nrstest = \"{workspace_version}\"\n"
        ),
    );
    manifest(
        &member,
        "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = { workspace = true }\n",
    );
    let found = check_manifest(krate.path(), &member.join("src/lib.rs")).map(|v| v.kind);
    assert_eq!(found.as_deref(), expected);
}

#[rstest]
fn a_workspace_dependency_nobody_declares_is_missing(krate: TempDir) {
    manifest(
        krate.path(),
        "[package]\nname = \"a\"\n[dev-dependencies]\nrstest = { workspace = true }\n",
    );
    assert_eq!(kind_of(krate.path()).as_deref(), Some("missing_rstest"));
}

#[rstest]
fn a_file_with_no_manifest_is_left_alone(krate: TempDir) {
    assert_eq!(kind_of(krate.path()), None);
}

#[rstest]
fn the_report_names_the_manifest_once_per_crate(krate: TempDir) {
    manifest(krate.path(), "[package]\nname = \"a\"\n");
    fs::write(krate.path().join("src/more.rs"), PLAIN_TEST).expect("more.rs");
    let report = check_rstest(krate.path(), &[]).expect("check");
    let missing: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.kind == "missing_rstest")
        .collect();
    assert_eq!(missing.len(), 1, "{:?}", report.violations);
    assert_eq!(missing[0].path, "Cargo.toml");
    assert!(missing[0].message.contains("0.28.0"));
}

#[rstest]
fn a_crate_without_plain_tests_needs_nothing(krate: TempDir) {
    manifest(krate.path(), "[package]\nname = \"a\"\n");
    fs::write(krate.path().join("src/lib.rs"), "pub fn f() {}\n").expect("lib.rs");
    assert!(check_rstest(krate.path(), &[]).expect("check").passed());
}
