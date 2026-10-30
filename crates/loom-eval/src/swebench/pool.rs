//! File-level pooling of chunk retrieval scores (DRAG-54 / DRAG-55).

use std::collections::HashMap;

/// How chunk retrieval scores are aggregated to rank whole files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolingStrategy {
    /// Score is the maximum score of any chunk in the file.
    Max,
    /// Score is the decaying sum of the top-3 chunks: score_0 + 0.5 * score_1 + 0.25 * score_2.
    DecayTop3,
}

/// Whether a file name is a numbered migration such as `0001_initial.py` (`^\d{4}_.*\.py$`).
fn is_numbered_migration(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 8 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'_' && name.ends_with(".py")
}

/// Whether a file path is a test or a numbered database migration.
///
/// Deliberately narrow so real source such as `django/test/testcases.py` or
/// `sphinx/testing/fixtures.py` is kept:
/// - the first directory is `tests`/`test`/`testing` (`testing` is pytest's own test dir), or the
///   parent directory is `tests`;
/// - the file name is `conftest.py`, `test_*.py`, `*_test.py`, `*_tests.py` or `tests.py`;
/// - the file is a numbered migration inside a `migrations` directory.
pub fn is_test_or_migration(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut p = lower.as_str();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    let mut comps: Vec<&str> = p.split('/').filter(|c| !c.is_empty()).collect();
    let Some(name) = comps.pop() else {
        return false;
    };
    let dirs = comps;

    if let Some(first) = dirs.first()
        && matches!(*first, "tests" | "test" | "testing")
    {
        return true;
    }
    if dirs.last() == Some(&"tests") {
        return true;
    }

    if name == "conftest.py"
        || name == "tests.py"
        || (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("_test.py")
        || name.ends_with("_tests.py")
    {
        return true;
    }

    dirs.contains(&"migrations") && is_numbered_migration(name)
}

/// Pools (chunk_path, score) hits into a sorted file ranking `(file_path, file_score)`.
pub fn pool_chunk_scores(
    hits: impl IntoIterator<Item = (String, f32)>,
    strategy: PoolingStrategy,
    exclude_tests_and_migrations: bool,
) -> Vec<(String, f32)> {
    let mut by_file: HashMap<String, Vec<f32>> = HashMap::new();
    for (path, score) in hits {
        if exclude_tests_and_migrations && is_test_or_migration(&path) {
            continue;
        }
        by_file.entry(path).or_default().push(score);
    }

    let mut ranked: Vec<(String, f32)> = by_file
        .into_iter()
        .map(|(path, mut scores)| {
            scores.sort_by(|a, b| b.total_cmp(a));
            let final_score = match strategy {
                PoolingStrategy::Max => scores.first().copied().unwrap_or(0.0),
                PoolingStrategy::DecayTop3 => {
                    let s0 = scores.first().copied().unwrap_or(0.0);
                    let s1 = scores.get(1).copied().unwrap_or(0.0);
                    let s2 = scores.get(2).copied().unwrap_or(0.0);
                    s0 + 0.5 * s1 + 0.25 * s2
                }
            };
            (path, final_score)
        })
        .collect();

    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pools_max_and_decay_top3() {
        let hits = vec![
            ("a.py".into(), 1.0),
            ("a.py".into(), 0.8),
            ("a.py".into(), 0.4),
            ("b.py".into(), 1.1),
        ];

        let max_ranked = pool_chunk_scores(hits.clone(), PoolingStrategy::Max, false);
        assert_eq!(max_ranked[0].0, "b.py");
        assert_eq!(max_ranked[0].1, 1.1);
        assert_eq!(max_ranked[1].0, "a.py");
        assert_eq!(max_ranked[1].1, 1.0);

        let decay_ranked = pool_chunk_scores(hits, PoolingStrategy::DecayTop3, false);
        // a.py: 1.0 + 0.5 * 0.8 + 0.25 * 0.4 = 1.0 + 0.4 + 0.1 = 1.5
        // b.py: 1.1
        assert_eq!(decay_ranked[0].0, "a.py");
        assert!((decay_ranked[0].1 - 1.5).abs() < 1e-6);
        assert_eq!(decay_ranked[1].0, "b.py");
        assert_eq!(decay_ranked[1].1, 1.1);
    }

    #[test]
    fn excludes_tests_and_migrations() {
        let hits = vec![
            ("django/core/models.py".into(), 0.9),
            ("tests/test_models.py".into(), 1.5),
            ("django/db/migrations/0001_initial.py".into(), 1.4),
        ];

        let ranked = pool_chunk_scores(hits, PoolingStrategy::Max, true);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "django/core/models.py");
    }

    #[test]
    fn keeps_real_source_that_looks_test_related() {
        for path in [
            "django/db/migrations/autodetector.py",
            "django/test/testcases.py",
            "sphinx/testing/fixtures.py",
            "sympy/utilities/pytest.py",
            "src/_pytest/unittest.py",
            "sphinx/ext/doctest.py",
        ] {
            assert!(!is_test_or_migration(path), "{path} must not be excluded");
        }
    }

    #[test]
    fn excludes_tests_and_numbered_migrations() {
        for path in [
            "django/contrib/auth/migrations/0001_initial.py",
            "tests/foo/test_bar.py",
            "testing/test_x.py",
            "conftest.py",
            "pkg/tests/helpers.py",
            "pkg/sub/test_thing.py",
            "pkg/thing_test.py",
            "pkg/thing_tests.py",
            "pkg/tests.py",
        ] {
            assert!(is_test_or_migration(path), "{path} must be excluded");
        }
    }
}
