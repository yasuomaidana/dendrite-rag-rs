//! Demonstration of retrieval evaluation metrics and bootstrap confidence intervals.
//!
//! Run with:
//!     cargo run --example quick_eval -p loom-eval

use std::time::Duration;

use loom_eval::golden::Query;
use loom_eval::metrics::{Scores, Unit};
use loom_eval::report;

/// Scores `query` against the ranked `hits` (`path`, `tokens`) a method returned in `millis` ms.
fn record(scores: &mut Scores, query: &Query, hits: &[(&str, usize)], millis: u64) {
    let units: Vec<Unit> = hits
        .iter()
        .map(|&(path, tokens)| Unit {
            path: path.into(),
            also: vec![],
            tokens,
        })
        .collect();
    scores.add(query, &units, None, Duration::from_millis(millis));
}

fn main() -> anyhow::Result<()> {
    println!("=== Loom-Eval Metrics & Confidence Interval Demo ===\n");

    // 1. Define synthetic queries with expectations
    let queries = [
        Query {
            q: "how does lexical bm25 scoring work?".into(),
            expect: vec!["crates/loom-rag/src/lexical.rs".into()],
        },
        Query {
            q: "where is reciprocal rank fusion implemented?".into(),
            expect: vec!["crates/loom-rag/src/fusion.rs".into()],
        },
        Query {
            q: "how does tree-sitter chunking split code?".into(),
            expect: vec!["crates/loom-rag/src/chunk.rs".into()],
        },
    ];

    // 2. Simulate retrieval results for Method A (e.g., BM25) and Method B (e.g., Hybrid)
    let mut scores_bm25 = Scores::default();
    let mut scores_hybrid = Scores::default();
    let (engine, lexical) = ("crates/loom-rag/src/engine.rs", "crates/loom-rag/src/lexical.rs");
    let (fusion, symbols, chunk) = (
        "crates/loom-rag/src/fusion.rs",
        "crates/loom-rag/src/symbols.rs",
        "crates/loom-rag/src/chunk.rs",
    );

    // Query 1: BM25 finds target at rank 2, Hybrid at rank 1
    record(&mut scores_bm25, &queries[0], &[(engine, 250), (lexical, 300)], 3);
    record(
        &mut scores_hybrid,
        &queries[0],
        &[(lexical, 300), (engine, 250)],
        6,
    );

    // Query 2: Both find at rank 1
    record(&mut scores_bm25, &queries[1], &[(fusion, 180)], 2);
    record(&mut scores_hybrid, &queries[1], &[(fusion, 180)], 5);

    // Query 3: BM25 misses in top 2, Hybrid finds at rank 1
    record(&mut scores_bm25, &queries[2], &[(symbols, 400)], 4);
    record(&mut scores_hybrid, &queries[2], &[(chunk, 320)], 7);

    // 3. Format into a comparison table with bootstrap CI
    let rows = vec![
        ("method: bm25".to_string(), scores_bm25),
        ("method: hybrid".to_string(), scores_hybrid),
    ];

    println!("{}", report::table("Synthetic Golden Set (3 queries)", &rows));

    Ok(())
}
