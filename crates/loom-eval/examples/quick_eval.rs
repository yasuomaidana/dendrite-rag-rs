//! Demonstration of retrieval evaluation metrics and bootstrap confidence intervals.
//!
//! Run with:
//!     cargo run --example quick_eval -p loom-eval

use std::time::Duration;

use loom_eval::golden::Query;
use loom_eval::metrics::{Scores, Unit};
use loom_eval::report;

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

    // Query 1: BM25 finds target at rank 2, Hybrid at rank 1
    scores_bm25.add(
        &queries[0],
        &[
            Unit {
                path: "crates/loom-rag/src/engine.rs".into(),
                also: vec![],
                tokens: 250,
            },
            Unit {
                path: "crates/loom-rag/src/lexical.rs".into(),
                also: vec![],
                tokens: 300,
            },
        ],
        None,
        Duration::from_millis(3),
    );
    scores_hybrid.add(
        &queries[0],
        &[
            Unit {
                path: "crates/loom-rag/src/lexical.rs".into(),
                also: vec![],
                tokens: 300,
            },
            Unit {
                path: "crates/loom-rag/src/engine.rs".into(),
                also: vec![],
                tokens: 250,
            },
        ],
        None,
        Duration::from_millis(6),
    );

    // Query 2: Both find at rank 1
    scores_bm25.add(
        &queries[1],
        &[Unit {
            path: "crates/loom-rag/src/fusion.rs".into(),
            also: vec![],
            tokens: 180,
        }],
        None,
        Duration::from_millis(2),
    );
    scores_hybrid.add(
        &queries[1],
        &[Unit {
            path: "crates/loom-rag/src/fusion.rs".into(),
            also: vec![],
            tokens: 180,
        }],
        None,
        Duration::from_millis(5),
    );

    // Query 3: BM25 misses in top 2, Hybrid finds at rank 1
    scores_bm25.add(
        &queries[2],
        &[Unit {
            path: "crates/loom-rag/src/symbols.rs".into(),
            also: vec![],
            tokens: 400,
        }],
        None,
        Duration::from_millis(4),
    );
    scores_hybrid.add(
        &queries[2],
        &[Unit {
            path: "crates/loom-rag/src/chunk.rs".into(),
            also: vec![],
            tokens: 320,
        }],
        None,
        Duration::from_millis(7),
    );

    // 3. Format into a comparison table with bootstrap CI
    let rows = vec![
        ("method: bm25".to_string(), scores_bm25),
        ("method: hybrid".to_string(), scores_hybrid),
    ];

    println!("{}", report::table("Synthetic Golden Set (3 queries)", &rows));

    Ok(())
}
