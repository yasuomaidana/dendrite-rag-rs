use super::*;
use std::fs;

fn run(repo: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    run(r, &["init", "-q", "-b", "main"]);
    fs::write(
        r.join("locate.go"),
        "package x\n\nfunc LocateBinary() string { return \"a\" }\n",
    )
    .unwrap();
    run(r, &["add", "."]);
    run(
        r,
        &[
            "commit",
            "-q",
            "-m",
            "feat: add binary lookup",
            "-m",
            "Searches the member checkouts.",
        ],
    );
    fs::write(
        r.join("locate.go"),
        "package x\n\nfunc LocateBinary() string { return \"b\" }\n",
    )
    .unwrap();
    fs::write(r.join("README.md"), "# x\n").unwrap();
    run(r, &["add", "."]);
    run(
        r,
        &[
            "commit",
            "-q",
            "-m",
            "fix: prefer the release build when locating sidecars",
        ],
    );
    dir
}

#[test]
fn parses_log_records() {
    let mut p = LogParser::default();
    let lines = [
        "\u{1e}abc\u{1f}100\u{1f}Ann\u{1f}feat: x",
        "body line",
        "\u{1f}",
        "",
        "diff --git a/src/a.rs b/src/a.rs",
        "index 1..2 100644",
        "--- a/src/a.rs",
        "+++ b/src/a.rs",
        "@@ -1 +1 @@",
        "-old",
        "+new",
        "\u{1e}def\u{1f}90\u{1f}Bob\u{1f}docs: y",
        "\u{1f}",
    ];
    let mut done: Vec<Commit> = lines.iter().filter_map(|l| p.push(l)).collect();
    done.extend(p.finish());
    assert_eq!(done.len(), 2);
    assert_eq!(
        (done[0].sha.as_str(), done[0].time, done[0].author.as_str()),
        ("abc", 100, "Ann")
    );
    assert_eq!(done[0].body, "body line");
    assert_eq!(
        done[0].files,
        vec![("src/a.rs".to_string(), "@@ -1 +1 @@\n-old\n+new\n".to_string())]
    );
    assert!(done[1].files.is_empty());
}

#[test]
fn indexes_incrementally_and_searches_messages_and_diffs() {
    let repo = repo();
    let dir = tempfile::tempdir().unwrap();
    let h = HistoryIndex::open(dir.path()).unwrap();
    let r = h.update(repo.path(), None, 100).unwrap();
    assert_eq!((r.commits, r.documents, r.rebuilt), (2, 3, true));

    let hits = h.search("why prefer release build sidecars", 3).unwrap();
    assert_eq!(
        hits[0].subject,
        "fix: prefer the release build when locating sidecars"
    );
    // The commit's README.md and locate.go documents tie on the message.
    let locate = hits.iter().find(|h| h.path == "locate.go").unwrap();
    assert_eq!(locate.sha, hits[0].sha);
    assert!(
        locate
            .diff
            .contains("+func LocateBinary() string { return \"b\" }")
    );
    assert!(
        h.search("member checkouts", 3).unwrap()[0]
            .subject
            .starts_with("feat")
    );

    // Nothing new: no work.
    let head = r.head.clone();
    let r = h.update(repo.path(), head.as_deref(), 100).unwrap();
    assert_eq!(r.commits, 0);

    fs::write(repo.path().join("new.rs"), "fn reciprocal_rank() {}\n").unwrap();
    run(repo.path(), &["add", "."]);
    run(repo.path(), &["commit", "-q", "-m", "feat: rank fusion"]);
    let r = h.update(repo.path(), head.as_deref(), 100).unwrap();
    assert_eq!((r.commits, r.rebuilt), (1, false));
    assert_eq!(h.len(), 4);
    assert_eq!(h.search("reciprocal_rank", 1).unwrap()[0].path, "new.rs");

    // An unknown base (e.g. after a rebase) rebuilds.
    let r = h
        .update(repo.path(), Some("0000000000000000000000000000000000000000"), 100)
        .unwrap();
    assert!(r.rebuilt);
    assert_eq!(h.len(), 4);
}

#[test]
fn non_repositories_error_and_empty_repositories_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let idx = tempfile::tempdir().unwrap();
    let h = HistoryIndex::open(idx.path()).unwrap();
    assert!(matches!(
        h.update(dir.path(), None, 10),
        Err(HistoryError::Git(_))
    ));
    run(dir.path(), &["init", "-q"]);
    assert_eq!(h.update(dir.path(), None, 10).unwrap(), HistoryReport::default());
}
