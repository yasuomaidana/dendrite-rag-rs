use std::collections::BTreeSet;

use rstest::{fixture, rstest};

use crate::quality::config::StructureConfig;
use crate::quality::docs::{is_manifest, missing_docs};
use crate::quality::structure::check_structure;
use crate::testutil::write_files;

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

#[fixture]
fn both() -> Vec<String> {
    vec!["README.md".into(), "CHANGELOG.md".into()]
}

#[rstest]
#[case::cargo_package("a/Cargo.toml", "[package]\nname = \"a\"", true)]
#[case::cargo_workspace_only("Cargo.toml", "[workspace]\nmembers = []", false)]
#[case::cargo_workspace_tables_only(
    "Cargo.toml",
    "[workspace.package]\nversion = \"1\"\n[workspace.dependencies]",
    false
)]
#[case::cargo_without_tables("x/Cargo.toml", "", true)]
#[case::go_module("svc/go.mod", "", true)]
#[case::npm("web/package.json", "", true)]
#[case::python("py/pyproject.toml", "", true)]
#[case::other("notes/main.rs", "", false)]
fn knows_a_unit_manifest(#[case] path: &str, #[case] text: &str, #[case] want: bool) {
    assert_eq!(is_manifest(path, &|_| text.to_string()), want);
}

#[rstest]
fn a_documented_unit_passes(both: Vec<String>) {
    let files = set(&[
        "README.md",
        "CHANGELOG.md",
        "crates/a/Cargo.toml",
        "crates/a/README.md",
        "crates/a/CHANGELOG.md",
    ]);
    assert!(missing_docs(&["crates/a/Cargo.toml".into()], &files, &both).is_empty());
}

#[rstest]
fn the_repository_root_is_always_a_unit(both: Vec<String>) {
    let found = missing_docs(&[], &set(&["src/lib.rs"]), &both);
    assert_eq!((found[0].path.as_str(), found[0].value), (".", 2));
    assert!(
        found[0]
            .message
            .starts_with("the repository root has no README.md and CHANGELOG.md")
    );
}

#[rstest]
fn names_only_what_is_missing(both: Vec<String>) {
    let files = set(&["README.md", "CHANGELOG.md", "crates/a/README.md"]);
    let found = missing_docs(&["crates/a/Cargo.toml".into()], &files, &both);
    assert_eq!(found.len(), 1);
    assert_eq!(
        (found[0].rule.as_str(), found[0].limit, found[0].files.clone()),
        ("docs", 0, vec!["crates/a/CHANGELOG.md".to_string()])
    );
}

#[test]
fn checks_a_real_tree() {
    let dir = write_files(&[
        ("README.md", "# p\n"),
        ("CHANGELOG.md", "# c\n"),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
        ),
        ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
        ("crates/a/README.md", "# a\n"),
        ("crates/a/CHANGELOG.md", "# a\n"),
        ("crates/a/src/lib.rs", "fn a() {}\n"),
        ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ("crates/b/src/lib.rs", "fn b() {}\n"),
    ]);
    let report = check_structure(dir.path(), &StructureConfig::default()).unwrap();
    let docs: Vec<_> = report.findings.iter().filter(|f| f.rule == "docs").collect();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].path, "crates/b");
    assert_eq!(docs[0].value, 2);

    let off = StructureConfig {
        require_docs: false,
        ..Default::default()
    };
    assert!(check_structure(dir.path(), &off).unwrap().passed());

    let only_readme = StructureConfig {
        docs_files: vec!["README.md".into()],
        ..Default::default()
    };
    assert_eq!(
        check_structure(dir.path(), &only_readme).unwrap().findings[0].value,
        1
    );

    let excluded = StructureConfig {
        exclude: vec!["crates/b/**".into()],
        ..Default::default()
    };
    assert!(check_structure(dir.path(), &excluded).unwrap().passed());
}

#[test]
fn rejects_docs_required_with_no_file_names() {
    let cfg = StructureConfig {
        docs_files: Vec::new(),
        ..Default::default()
    };
    assert!(cfg.validate().is_err());
}
