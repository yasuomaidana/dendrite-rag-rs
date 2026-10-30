//! Serialization of evaluation runs and paired comparisons between runs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::metrics::QueryScore;
use crate::stats;

/// A serialized evaluation run over multiple golden sets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunFile {
    pub root: String,
    pub sets: Vec<SetRun>,
}

/// Results for one golden query set in a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRun {
    pub name: String,
    pub methods: Vec<MethodRun>,
}

/// Results for one evaluation method on a golden set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodRun {
    pub label: String,
    pub per_query: Vec<QueryScore>,
}

/// A row in the comparison table between two evaluation runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonRow {
    pub set: String,
    pub method: String,
    pub n: usize,
    pub mrr_diff: f64,
    pub mrr_ci: (f64, f64),
    pub ndcg_diff: f64,
    pub ndcg_ci: (f64, f64),
    pub significant: bool,
}

/// Compares two run files and computes paired difference confidence intervals
/// for every (set, method) present in both runs.
pub fn compare_runs(a: &RunFile, b: &RunFile) -> Vec<ComparisonRow> {
    let mut rows = Vec::new();
    for set_a in &a.sets {
        let Some(set_b) = b.sets.iter().find(|s| s.name == set_a.name) else {
            continue;
        };
        for method_a in &set_a.methods {
            let Some(method_b) = set_b.methods.iter().find(|m| m.label == method_a.label) else {
                continue;
            };

            let mut b_map: HashMap<&str, Vec<&QueryScore>> = HashMap::new();
            for qb in &method_b.per_query {
                b_map.entry(qb.q.as_str()).or_default().push(qb);
            }

            let mut a_rr = Vec::new();
            let mut b_rr = Vec::new();
            let mut a_ndcg = Vec::new();
            let mut b_ndcg = Vec::new();

            for qa in &method_a.per_query {
                if let Some(list) = b_map.get_mut(qa.q.as_str())
                    && !list.is_empty()
                {
                    let qb = list.remove(0);
                    a_rr.push(qa.rr);
                    b_rr.push(qb.rr);
                    a_ndcg.push(qa.ndcg);
                    b_ndcg.push(qb.ndcg);
                }
            }

            let n = a_rr.len();
            let (mrr_diff, mrr_lo, mrr_hi) =
                stats::paired_diff_ci(&a_rr, &b_rr, stats::RESAMPLES, stats::SEED);
            let (ndcg_diff, ndcg_lo, ndcg_hi) =
                stats::paired_diff_ci(&a_ndcg, &b_ndcg, stats::RESAMPLES, stats::SEED);
            let significant = mrr_lo > 0.0 || mrr_hi < 0.0;

            rows.push(ComparisonRow {
                set: set_a.name.clone(),
                method: method_a.label.clone(),
                n,
                mrr_diff,
                mrr_ci: (mrr_lo, mrr_hi),
                ndcg_diff,
                ndcg_ci: (ndcg_lo, ndcg_hi),
                significant,
            });
        }
    }
    rows
}

/// Formats comparison rows as a Markdown table.
pub fn format_table(rows: &[ComparisonRow]) -> String {
    let mut out = String::from(
        "| set | method | n | ΔMRR@10 [95% CI] | ΔnDCG@10 [95% CI] | significant |\n\
         |---|---|---|---|---|---|\n",
    );
    for r in rows {
        let sig = if r.significant { "yes" } else { "no" };
        out.push_str(&format!(
            "| {} | {} | {} | {:.3} [{:.3}, {:.3}] | {:.3} [{:.3}, {:.3}] | {} |\n",
            r.set,
            r.method,
            r.n,
            norm(r.mrr_diff),
            norm(r.mrr_ci.0),
            norm(r.mrr_ci.1),
            norm(r.ndcg_diff),
            norm(r.ndcg_ci.0),
            norm(r.ndcg_ci.1),
            sig
        ));
    }
    out
}

/// Compares two runs and returns the formatted Markdown table.
pub fn compare(a: &RunFile, b: &RunFile) -> String {
    format_table(&compare_runs(a, b))
}

fn norm(x: f64) -> f64 {
    if x.abs() < 1e-12 { 0.0 } else { x }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A query scored `value` on both rank metrics, found (`hit`) or not.
    fn score(q: &str, value: f64, hit: bool) -> QueryScore {
        QueryScore {
            q: q.into(),
            rr: value,
            ndcg: value,
            recall: vec![hit],
        }
    }

    /// A run of the `dendrite` set whose `search` method scored `per_query`.
    fn run_of(per_query: Vec<QueryScore>) -> RunFile {
        RunFile {
            root: "root".into(),
            sets: vec![SetRun {
                name: "dendrite".into(),
                methods: vec![MethodRun {
                    label: "search".into(),
                    per_query,
                }],
            }],
        }
    }

    #[test]
    fn compare_matching_ignores_queries_missing_from_one_side() {
        let a = run_of(vec![score("q1", 0.5, true), score("q2", 1.0, true)]);
        let b = run_of(vec![score("q2", 1.0, true), score("q3", 0.2, false)]);

        let rows = compare_runs(&a, &b);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].set, "dendrite");
        assert_eq!(rows[0].method, "search");
        assert_eq!(rows[0].n, 1);
        assert!((rows[0].mrr_diff - 0.0).abs() < 1e-9);
        assert!(!rows[0].significant);

        let table = compare(&a, &b);
        assert!(
            table.contains("| dendrite | search | 1 | 0.000 [0.000, 0.000] | 0.000 [0.000, 0.000] | no |")
        );
    }

    #[test]
    fn compare_reports_significance_when_ci_excludes_zero() {
        let a = run_of(vec![score("q1", 0.0, false), score("q2", 0.1, false)]);
        let b = run_of(vec![score("q1", 1.0, true), score("q2", 1.0, true)]);

        let rows = compare_runs(&a, &b);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].significant);
        let table = compare(&a, &b);
        assert!(table.contains("| yes |"));
    }
}
