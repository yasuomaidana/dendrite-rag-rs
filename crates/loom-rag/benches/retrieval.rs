//! Wall-clock benchmarks of the query path, the baseline later changes are measured
//! against (`eval/RESULTS.md` § Benchmarks):
//!
//!     cargo bench -p loom-rag --features testutil --bench retrieval

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use loom_rag::engine::{Config, Engine};
use loom_rag::fusion::{RRF_K, rrf};
use loom_rag::retrieve::{Mode, SearchOpts};
use loom_rag::testutil::{HashEmbedder, synthetic_project};
use loom_rag::vector::{FlatIndex, VectorIndex};

/// Files in the benchmark corpus (four functions each).
const FILES: usize = 2_000;
const QUERIES: [&str; 4] = [
    "parse the ledger before commit",
    "RetryBudgetWindow",
    "how does the worker lease a queue batch",
    "fusion score rank",
];

fn engine() -> (tempfile::TempDir, Engine) {
    let dir = synthetic_project(FILES);
    let mut e =
        Engine::open_with_embedder(dir.path(), Config::default(), Some(Box::new(HashEmbedder::new())))
            .expect("open");
    e.index(false).expect("index");
    (dir, e)
}

fn query_path(c: &mut Criterion) {
    let (_dir, e) = engine();
    let mut g = c.benchmark_group("query");
    g.bench_function("lexical", |b| {
        b.iter(|| {
            for q in QUERIES {
                black_box(e.search_lexical(q, 10).unwrap());
            }
        })
    });
    for (name, mode) in [("dense", Mode::Dense), ("hybrid", Mode::Hybrid)] {
        let opts = SearchOpts {
            mode,
            ..SearchOpts::default()
        };
        g.bench_function(name, |b| {
            b.iter(|| {
                for q in QUERIES {
                    black_box(e.search(q, &opts).unwrap());
                }
            })
        });
    }
    g.bench_function("context_2500", |b| {
        b.iter(|| {
            for q in QUERIES {
                black_box(e.context(q, 2_500).unwrap());
            }
        })
    });
    g.finish();
}

fn flat_scan(c: &mut Criterion) {
    let mut state = 1u64;
    let mut unit = || {
        let v: Vec<f32> = (0..256)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            })
            .collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect::<Vec<f32>>()
    };
    let mut idx = FlatIndex::new();
    for i in 0..50_000 {
        idx.upsert(&format!("c{i}"), "t", &unit());
    }
    let q = unit();
    c.bench_function("flat_scan_50k_256d_top40", |b| {
        b.iter(|| black_box(idx.search(&q, 40)))
    });
}

fn fusion(c: &mut Criterion) {
    let list = |offset: usize| -> Vec<(String, f32)> {
        (0..100)
            .map(|i| (format!("c{}", (i * 7 + offset) % 150), 1.0))
            .collect()
    };
    let (a, b, s) = (list(0), list(3), list(11));
    c.bench_function("rrf_3x100", |bench| {
        bench.iter(|| black_box(rrf(&[(1.0, &a), (1.0, &b), (0.5, &s)], RRF_K)))
    });
}

criterion_group!(benches, query_path, flat_scan, fusion);
criterion_main!(benches);
