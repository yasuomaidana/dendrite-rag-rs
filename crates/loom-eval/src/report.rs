//! Markdown tables for `eval/RESULTS.md`.

use crate::metrics::{BUDGETS, Scores};
use crate::stats;

pub fn table(title: &str, rows: &[(String, Scores)]) -> String {
    let mut out = format!("### {title}\n\n| method | hit@1 | hit@5 | hit@10 | MRR@10 | nDCG@10 |");
    for b in BUDGETS {
        out.push_str(&format!(" recall@{}k tok |", fmt_k(b)));
    }
    out.push_str(" median tokens to answer | both@2.5k | p50 latency |\n|---|---|---|---|---|");
    out.push_str(&"---|".repeat(BUDGETS.len() + 1));
    out.push_str("---|---|---|\n");
    for (label, s) in rows {
        let rrs: Vec<f64> = s.per_query.iter().map(|q| q.rr).collect();
        let (mrr_lo, mrr_hi) = stats::bootstrap_ci(&rrs, stats::RESAMPLES, stats::SEED);
        let ndcgs: Vec<f64> = s.per_query.iter().map(|q| q.ndcg).collect();
        let (ndcg_lo, ndcg_hi) = stats::bootstrap_ci(&ndcgs, stats::RESAMPLES, stats::SEED);

        out.push_str(&format!(
            "| {label} | {:.2} | {:.2} | {:.2} | {:.3} [{:.3}, {:.3}] | {:.3} [{:.3}, {:.3}] |",
            s.rate(s.hit1),
            s.rate(s.hit5),
            s.rate(s.hit10),
            s.mrr(),
            mrr_lo,
            mrr_hi,
            s.ndcg10(),
            ndcg_lo,
            ndcg_hi
        ));
        for r in s.recall {
            out.push_str(&format!(" {:.2} |", s.rate(r)));
        }
        let median = s.median_tokens().map_or("—".to_string(), |t| {
            format!("{t} ({:.0}% found)", s.rate(s.tokens_to_first.len()) * 100.0)
        });
        let both = if s.mixed > 0 {
            format!("{:.2}", s.both as f64 / s.mixed as f64)
        } else {
            "—".to_string()
        };
        out.push_str(&format!(
            " {median} | {both} | {:.1} ms |\n",
            s.p50_latency().as_secs_f64() * 1000.0
        ));
    }
    out
}

fn fmt_k(tokens: usize) -> String {
    let k = tokens as f64 / 1000.0;
    if k.fract() == 0.0 {
        format!("{k:.0}")
    } else {
        format!("{k:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::QueryScore;

    #[test]
    fn table_formats_bootstrap_ci() {
        let s = Scores {
            n: 1,
            rr: 0.821,
            ndcg: 0.810,
            per_query: vec![QueryScore {
                q: "q1".into(),
                rr: 0.821,
                ndcg: 0.810,
                recall: vec![true, true, true],
            }],
            ..Scores::default()
        };
        let t = table("test", &[("search".into(), s)]);
        assert!(t.contains("| search | 0.00 | 0.00 | 0.00 | 0.821 [0.821, 0.821] | 0.810 [0.810, 0.810] |"));
    }
}
