//! Quality and token metrics over a method's ranked output.
//!
//! A method returns [`Unit`]s, best first: chunks, context spans or whole files. Quality is
//! scored on the files in that order (hit@k, MRR@10). Token cost is what a consumer reads:
//! units are taken in order while they fit a budget (a unit that does not fit is skipped,
//! like the context packer does), and `recall@B` is the share of queries whose expected
//! file is among them.

use std::time::Duration;

use crate::golden::{Query, is_doc};

/// Token budgets the recall is measured at.
pub const BUDGETS: [usize; 3] = [1000, 2500, 4000];
/// The budget the mixed-question `both` metric uses.
pub const BOTH_BUDGET: usize = 2500;

/// One retrieved unit.
#[derive(Debug, Clone)]
pub struct Unit {
    pub path: String,
    /// Files holding the same text, folded into this unit by de-duplication.
    pub also: Vec<String>,
    pub tokens: usize,
}

impl Unit {
    /// Whether this unit carries the text of `path`.
    pub fn holds(&self, wanted: impl Fn(&str) -> bool) -> bool {
        wanted(&self.path) || self.also.iter().any(|p| wanted(p))
    }
}

/// Detailed evaluation scores and recall flags for a single query.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueryScore {
    pub q: String,
    pub rr: f64,
    pub ndcg: f64,
    pub recall: Vec<bool>,
}

#[derive(Debug, Default, Clone)]
pub struct Scores {
    pub n: usize,
    pub hit1: usize,
    pub hit5: usize,
    pub hit10: usize,
    pub rr: f64,
    /// Binary-relevance nDCG@10 over files (the CoIR metric).
    pub ndcg: f64,
    pub recall: [usize; BUDGETS.len()],
    /// Tokens read up to and including the first expected file, for queries where it was found.
    pub tokens_to_first: Vec<usize>,
    /// Mixed questions with an expected doc and an expected code file within [`BOTH_BUDGET`].
    pub both: usize,
    pub mixed: usize,
    pub latency: Vec<Duration>,
    pub misses: Vec<String>,
    pub per_query: Vec<QueryScore>,
}

/// The units a consumer reads within `budget` tokens.
pub fn within(units: &[Unit], budget: usize) -> Vec<&Unit> {
    let mut used = 0;
    let mut out = Vec::new();
    for u in units {
        if used + u.tokens <= budget {
            used += u.tokens;
            out.push(u);
        }
    }
    out
}

impl Scores {
    /// Scores ranked `units`; `at_budget[i]` holds the units for `BUDGETS[i]` when the method
    /// packs per budget (context packs), otherwise `units` is cut to each budget.
    pub fn add(&mut self, q: &Query, units: &[Unit], at_budget: Option<&[Vec<Unit>]>, elapsed: Duration) {
        self.n += 1;
        self.latency.push(elapsed);
        let wanted = |p: &str| q.expect.iter().any(|e| e == p);

        let mut files: Vec<&Unit> = Vec::new();
        for u in units {
            if !files.iter().any(|f| f.path == u.path) {
                files.push(u);
            }
        }
        let query_rr = match files.iter().take(10).position(|u| u.holds(wanted)) {
            Some(i) => {
                self.hit1 += (i < 1) as usize;
                self.hit5 += (i < 5) as usize;
                self.hit10 += 1;
                let r = 1.0 / (i as f64 + 1.0);
                self.rr += r;
                r
            }
            None => {
                self.misses.push(q.q.clone());
                0.0
            }
        };
        // Each expected file earns its gain once, however many copies of it are listed.
        let mut credited: Vec<&str> = Vec::new();
        let mut dcg = 0.0;
        for (i, u) in files.iter().take(10).enumerate() {
            let fresh = q
                .expect
                .iter()
                .find(|e| !credited.contains(&e.as_str()) && u.holds(|p| p == e.as_str()));
            if let Some(e) = fresh {
                credited.push(e);
                dcg += 1.0 / (i as f64 + 2.0).log2();
            }
        }
        let ideal: f64 = (0..q.expect.len().min(10))
            .map(|i| 1.0 / (i as f64 + 2.0).log2())
            .sum();
        let query_ndcg = dcg / ideal.max(f64::MIN_POSITIVE);
        self.ndcg += query_ndcg;

        let mut read = 0;
        for u in units {
            read += u.tokens;
            if u.holds(wanted) {
                self.tokens_to_first.push(read);
                break;
            }
        }

        let mut query_recall = Vec::with_capacity(BUDGETS.len());
        for (i, &b) in BUDGETS.iter().enumerate() {
            let seen: Vec<&Unit> = match at_budget {
                Some(per) => per[i].iter().collect(),
                None => within(units, b),
            };
            let hit = seen.iter().any(|u| u.holds(wanted));
            self.recall[i] += hit as usize;
            query_recall.push(hit);
            if b == BOTH_BUDGET && q.is_mixed() {
                self.mixed += 1;
                let doc = seen.iter().any(|u| u.holds(|p| wanted(p) && is_doc(p)));
                let code = seen.iter().any(|u| u.holds(|p| wanted(p) && !is_doc(p)));
                self.both += (doc && code) as usize;
            }
        }
        self.per_query.push(QueryScore {
            q: q.q.clone(),
            rr: query_rr,
            ndcg: query_ndcg,
            recall: query_recall,
        });
    }

    pub fn mrr(&self) -> f64 {
        self.rr / self.n.max(1) as f64
    }

    pub fn ndcg10(&self) -> f64 {
        self.ndcg / self.n.max(1) as f64
    }

    pub fn rate(&self, k: usize) -> f64 {
        k as f64 / self.n.max(1) as f64
    }

    /// Median tokens to the first expected file over the queries where it was found.
    pub fn median_tokens(&self) -> Option<usize> {
        let mut v = self.tokens_to_first.clone();
        v.sort_unstable();
        v.get(v.len() / 2).copied()
    }

    pub fn p50_latency(&self) -> Duration {
        let mut v = self.latency.clone();
        v.sort_unstable();
        v.get(v.len() / 2).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(path: &str, tokens: usize) -> Unit {
        Unit {
            path: path.into(),
            also: vec![],
            tokens,
        }
    }

    #[test]
    fn ranks_files_and_charges_tokens() {
        let q = Query {
            q: "x".into(),
            expect: vec!["b.go".into(), "doc.md".into()],
        };
        let units = [
            unit("a.go", 900),
            unit("a.go", 50),
            unit("b.go", 300),
            unit("doc.md", 3000),
        ];
        let mut s = Scores::default();
        s.add(&q, &units, None, Duration::ZERO);
        assert_eq!((s.hit1, s.hit5), (0, 1));
        assert!((s.mrr() - 0.5).abs() < 1e-9, "b.go is the second file");
        assert_eq!(s.tokens_to_first, vec![1250]);
        let ideal = 1.0 + 1.0 / 3f64.log2();
        assert!((s.ndcg10() - (1.0 / 3f64.log2() + 1.0 / 4f64.log2()) / ideal).abs() < 1e-9);
        assert_eq!(s.recall, [0, 1, 1], "b.go fits from 2.5k on");
        assert_eq!(
            (s.mixed, s.both),
            (1, 0),
            "doc.md does not fit with the code at 2.5k"
        );
        assert_eq!(s.per_query.len(), 1);
        assert_eq!(s.per_query[0].q, "x");
        assert!((s.per_query[0].rr - 0.5).abs() < 1e-9);
        assert_eq!(s.per_query[0].recall, vec![false, true, true]);
    }

    #[test]
    fn copies_of_one_expected_file_earn_its_gain_once() {
        let q = Query {
            q: "x".into(),
            expect: vec!["a.py".into()],
        };
        let copy = |path: &str, also: &str| Unit {
            path: path.into(),
            also: vec![also.into()],
            tokens: 10,
        };
        let mut s = Scores::default();
        s.add(
            &q,
            &[copy("a.py", "b.py"), copy("b.py", "a.py")],
            None,
            Duration::ZERO,
        );
        assert!(
            (s.ndcg10() - 1.0).abs() < 1e-9,
            "nDCG stays within 1: {}",
            s.ndcg10()
        );
    }
}
