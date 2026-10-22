//! Markdown tables for `eval/RESULTS.md`.

use crate::metrics::{BUDGETS, Scores};

pub fn table(title: &str, rows: &[(String, Scores)]) -> String {
    let mut out = format!("### {title}\n\n| method | hit@1 | hit@5 | hit@10 | MRR@10 | nDCG@10 |");
    for b in BUDGETS {
        out.push_str(&format!(" recall@{}k tok |", fmt_k(b)));
    }
    out.push_str(" median tokens to answer | both@2.5k | p50 latency |\n|---|---|---|---|---|");
    out.push_str(&"---|".repeat(BUDGETS.len() + 1));
    out.push_str("---|---|---|\n");
    for (label, s) in rows {
        out.push_str(&format!(
            "| {label} | {:.2} | {:.2} | {:.2} | {:.3} | {:.3} |",
            s.rate(s.hit1),
            s.rate(s.hit5),
            s.rate(s.hit10),
            s.mrr(),
            s.ndcg10()
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
