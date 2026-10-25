//! SWE-bench evaluation metrics: Hit@k, All-gold@k, gold-file recall@10, MRR@10, nDCG@10 and
//! repo-macro averaging.
//!
//! Hit@k is the share of instances with *any* gold file in the top k; gold-file recall@10 is the
//! mean fraction of distinct gold files found in the top 10. A ranked path is gold only on exact
//! equality with an expected path (repo-relative, slash-separated, leading `./` ignored).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::instance::{LeakageClass, SweInstance};

/// Strips leading `./` segments so paths compare as repo-relative.
pub fn normalize_path(path: &str) -> &str {
    let mut p = path;
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    p
}

/// Whether `path` exactly equals one of the `expected` paths (after `./` normalization).
pub fn path_is_gold(expected: &[String], path: &str) -> bool {
    let path = normalize_path(path);
    expected.iter().any(|e| normalize_path(e) == path)
}

/// Alias for [`path_is_gold`]: exact equality after stripping leading `./` on both paths.
pub fn is_gold(expected: &[String], path: &str) -> bool {
    path_is_gold(expected, path)
}

/// Evaluation score for one query on one SWE-bench instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceScore {
    pub instance_id: String,
    pub repo: String,
    pub leakage_class: LeakageClass,
    /// Hit@1: a gold file is ranked first (hit-rate semantics).
    pub recall1: bool,
    /// Hit@5: any gold file is in the top 5 (hit-rate semantics).
    pub recall5: bool,
    /// Hit@10: any gold file is in the top 10 (hit-rate semantics).
    pub recall10: bool,
    /// Whether all gold files were in top 5 (None for single-file instances).
    pub all_gold5: Option<bool>,
    /// Whether all gold files were in top 10 (None for single-file instances).
    pub all_gold10: Option<bool>,
    /// Fraction of distinct gold files within the top 10.
    #[serde(default)]
    pub gold_recall10: f64,
    pub mrr10: f64,
    /// Binary-relevance nDCG@10 over distinct gold files.
    #[serde(default)]
    pub ndcg10: f64,
    pub tokens_to_gold: Option<usize>,
}

/// Distinct gold files found within the first `k` ranked paths.
fn gold_in_top<'a>(ranked: &[&'a str], gold: &HashSet<&str>, k: usize) -> HashSet<&'a str> {
    ranked
        .iter()
        .take(k)
        .filter(|p| gold.contains(**p))
        .copied()
        .collect()
}

/// Binary nDCG@10 where each distinct gold file counts once, at its first rank.
fn ndcg_at_10(ranked: &[&str], gold: &HashSet<&str>) -> f64 {
    if gold.is_empty() {
        return 0.0;
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dcg = 0.0;
    for (i, p) in ranked.iter().take(10).enumerate() {
        if gold.contains(*p) && seen.insert(*p) {
            dcg += 1.0 / (i as f64 + 2.0).log2();
        }
    }
    let ideal: f64 = (0..gold.len().min(10))
        .map(|i| 1.0 / (i as f64 + 2.0).log2())
        .sum();
    dcg / ideal
}

impl InstanceScore {
    pub fn compute(instance: &SweInstance, ranked_files: &[String], tokens_to_gold: Option<usize>) -> Self {
        let gold: HashSet<&str> = instance
            .expected_files
            .iter()
            .map(|e| normalize_path(e))
            .collect();
        let ranked: Vec<&str> = ranked_files.iter().map(|p| normalize_path(p)).collect();

        let top1 = ranked.first().is_some_and(|p| gold.contains(p));
        let found5 = gold_in_top(&ranked, &gold, 5);
        let found10 = gold_in_top(&ranked, &gold, 10);

        let (all_gold5, all_gold10) = if gold.len() > 1 {
            (
                Some(found5.len() == gold.len()),
                Some(found10.len() == gold.len()),
            )
        } else {
            (None, None)
        };

        let gold_recall10 = if gold.is_empty() {
            0.0
        } else {
            found10.len() as f64 / gold.len() as f64
        };

        let mut mrr10 = 0.0;
        for (i, p) in ranked.iter().take(10).enumerate() {
            if gold.contains(p) {
                mrr10 = 1.0 / (i as f64 + 1.0);
                break;
            }
        }

        Self {
            instance_id: instance.instance_id.clone(),
            repo: instance.repo.clone(),
            leakage_class: instance.leakage_class,
            recall1: top1,
            recall5: !found5.is_empty(),
            recall10: !found10.is_empty(),
            all_gold5,
            all_gold10,
            gold_recall10,
            mrr10,
            ndcg10: ndcg_at_10(&ranked, &gold),
            tokens_to_gold,
        }
    }
}

/// Summary metrics aggregated over a collection of instances.
///
/// The `recall*` fields hold Hit@k rates (any gold file in the top k; hit-rate semantics).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AggregatedMetrics {
    pub n: usize,
    /// Hit@1: fraction of instances with any gold file ranked 1st (hit-rate semantics).
    pub recall1: f64,
    /// Hit@5: fraction of instances with any gold file in the top 5 (hit-rate semantics).
    pub recall5: f64,
    /// Hit@10: fraction of instances with any gold file in the top 10 (hit-rate semantics).
    pub recall10: f64,
    pub all_gold5: Option<f64>,
    pub all_gold10: Option<f64>,
    /// Mean fraction of distinct gold files within the top 10.
    pub gold_recall10: f64,
    pub mrr10: f64,
    pub median_tokens_to_gold: Option<usize>,
}

impl AggregatedMetrics {
    pub fn aggregate(scores: &[InstanceScore]) -> Self {
        if scores.is_empty() {
            return Self::default();
        }
        let n = scores.len();
        let r1 = scores.iter().filter(|s| s.recall1).count() as f64 / n as f64;
        let r5 = scores.iter().filter(|s| s.recall5).count() as f64 / n as f64;
        let r10 = scores.iter().filter(|s| s.recall10).count() as f64 / n as f64;
        let gold_r10 = scores.iter().map(|s| s.gold_recall10).sum::<f64>() / n as f64;
        let mrr = scores.iter().map(|s| s.mrr10).sum::<f64>() / n as f64;

        let multi: Vec<&InstanceScore> = scores.iter().filter(|s| s.all_gold5.is_some()).collect();
        let (all_gold5, all_gold10) = if !multi.is_empty() {
            let m_n = multi.len() as f64;
            let g5 = multi.iter().filter(|s| s.all_gold5 == Some(true)).count() as f64 / m_n;
            let g10 = multi.iter().filter(|s| s.all_gold10 == Some(true)).count() as f64 / m_n;
            (Some(g5), Some(g10))
        } else {
            (None, None)
        };

        let mut tokens: Vec<usize> = scores.iter().filter_map(|s| s.tokens_to_gold).collect();
        tokens.sort_unstable();
        let median_tokens = if !tokens.is_empty() {
            Some(tokens[tokens.len() / 2])
        } else {
            None
        };

        Self {
            n,
            recall1: r1,
            recall5: r5,
            recall10: r10,
            all_gold5,
            all_gold10,
            gold_recall10: gold_r10,
            mrr10: mrr,
            median_tokens_to_gold: median_tokens,
        }
    }
}

/// Computes both micro-averaged and repo-macro-averaged metrics.
pub fn summarize_report(
    scores: &[InstanceScore],
) -> (
    AggregatedMetrics,
    AggregatedMetrics,
    BTreeMap<String, AggregatedMetrics>,
    HashMap<LeakageClass, AggregatedMetrics>,
) {
    let micro = AggregatedMetrics::aggregate(scores);

    // Group by repo
    let mut by_repo: BTreeMap<String, Vec<InstanceScore>> = BTreeMap::new();
    for s in scores {
        by_repo.entry(s.repo.clone()).or_default().push(s.clone());
    }

    let repo_summaries: BTreeMap<String, AggregatedMetrics> = by_repo
        .iter()
        .map(|(repo, scs)| (repo.clone(), AggregatedMetrics::aggregate(scs)))
        .collect();

    // Repo-macro average
    let repo_macro = if !repo_summaries.is_empty() {
        let num_repos = repo_summaries.len() as f64;
        AggregatedMetrics {
            n: scores.len(),
            recall1: repo_summaries.values().map(|s| s.recall1).sum::<f64>() / num_repos,
            recall5: repo_summaries.values().map(|s| s.recall5).sum::<f64>() / num_repos,
            recall10: repo_summaries.values().map(|s| s.recall10).sum::<f64>() / num_repos,
            all_gold5: {
                let with_multi: Vec<_> = repo_summaries.values().filter_map(|s| s.all_gold5).collect();
                if with_multi.is_empty() {
                    None
                } else {
                    Some(with_multi.iter().sum::<f64>() / with_multi.len() as f64)
                }
            },
            all_gold10: {
                let with_multi: Vec<_> = repo_summaries.values().filter_map(|s| s.all_gold10).collect();
                if with_multi.is_empty() {
                    None
                } else {
                    Some(with_multi.iter().sum::<f64>() / with_multi.len() as f64)
                }
            },
            gold_recall10: repo_summaries.values().map(|s| s.gold_recall10).sum::<f64>() / num_repos,
            mrr10: repo_summaries.values().map(|s| s.mrr10).sum::<f64>() / num_repos,
            median_tokens_to_gold: micro.median_tokens_to_gold,
        }
    } else {
        AggregatedMetrics::default()
    };

    // By leakage class
    let mut by_leakage: HashMap<LeakageClass, Vec<InstanceScore>> = HashMap::new();
    for s in scores {
        by_leakage.entry(s.leakage_class).or_default().push(s.clone());
    }
    let leakage_summaries: HashMap<LeakageClass, AggregatedMetrics> = by_leakage
        .into_iter()
        .map(|(lc, scs)| (lc, AggregatedMetrics::aggregate(&scs)))
        .collect();

    (micro, repo_macro, repo_summaries, leakage_summaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(expected: &[&str]) -> SweInstance {
        SweInstance {
            instance_id: "a__b-1".into(),
            repo: "a/b".into(),
            base_commit: "abc".into(),
            created_at: None,
            expected_files: expected.iter().map(|s| s.to_string()).collect(),
            q1: String::new(),
            q2: String::new(),
            q3: String::new(),
            leakage_class: LeakageClass::Opaque,
            stratified_120: false,
        }
    }

    fn ranked(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn suffix_matches_are_not_gold() {
        let inst = instance(&["pkg/utils.py"]);
        let score = InstanceScore::compute(&inst, &ranked(&["other/pkg/utils.py", "utils.py"]), None);
        assert!(!score.recall10);
        assert_eq!(score.mrr10, 0.0);
        assert_eq!(score.gold_recall10, 0.0);
        assert_eq!(score.ndcg10, 0.0);
    }

    #[test]
    fn leading_dot_slash_is_ignored_on_both_sides() {
        let inst = instance(&["./pkg/a.py"]);
        let score = InstanceScore::compute(&inst, &ranked(&["x.py", "./pkg/a.py"]), None);
        assert!(score.recall5 && !score.recall1);
        assert!((score.mrr10 - 0.5).abs() < 1e-9);
        let score = InstanceScore::compute(&inst, &ranked(&["pkg/a.py"]), None);
        assert!(score.recall1);
    }

    #[test]
    fn gold_recall_and_all_gold_use_distinct_files() {
        let inst = instance(&["a.py", "b.py"]);
        let score = InstanceScore::compute(&inst, &ranked(&["a.py", "a.py", "c.py"]), None);
        assert_eq!(score.all_gold10, Some(false));
        assert!((score.gold_recall10 - 0.5).abs() < 1e-9);

        let score = InstanceScore::compute(&inst, &ranked(&["a.py", "c.py", "b.py"]), None);
        assert_eq!(score.all_gold5, Some(true));
        assert!((score.gold_recall10 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn exact_path_equality_helpers() {
        let expected = vec!["./pkg/a.py".to_string(), "b.py".to_string()];
        assert!(path_is_gold(&expected, "pkg/a.py"));
        assert!(path_is_gold(&expected, "./pkg/a.py"));
        assert!(path_is_gold(&expected, "./b.py"));
        assert!(path_is_gold(&expected, "b.py"));
        assert!(!path_is_gold(&expected, "other/pkg/a.py"));
        assert!(!path_is_gold(&expected, "a.py"));
        assert!(is_gold(&expected, "pkg/a.py"));
        assert!(!is_gold(&expected, "a.py"));
    }

    #[test]
    fn ndcg_is_one_for_perfect_ranking_and_discounts_later_hits() {
        let inst = instance(&["a.py", "b.py"]);
        let perfect = InstanceScore::compute(&inst, &ranked(&["a.py", "b.py"]), None);
        assert!((perfect.ndcg10 - 1.0).abs() < 1e-9);
        let late = InstanceScore::compute(&inst, &ranked(&["a.py", "x.py", "b.py"]), None);
        assert!(late.ndcg10 < 1.0 && late.ndcg10 > 0.0);
    }
}
