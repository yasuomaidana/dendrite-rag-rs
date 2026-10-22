//! Flat int8 scan against HNSW on clustered unit vectors: build time, per-query latency
//! (single reader and 8 concurrent) and recall of the engine's rescored 40-candidate
//! shortlist, at several corpus sizes (DRAG-41).
//!
//!     cargo run --release --features hnsw --example vector_bench -- [dim] [sizes...]
//!     cargo run --release --features hnsw --example vector_bench -- 768 50000 100000 200000
//!
//! `BENCH_FLAT_ONLY=1` skips HNSW (its build is the slow part).

use std::time::{Duration, Instant};

use loom_rag::vector::{FlatIndex, HnswIndex, VectorIndex};
use rayon::prelude::*;

/// Topic centres plus noise, normalized: real embeddings cluster around topics.
fn clustered(n: usize, dim: usize, clusters: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed;
    let mut next = move || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
    };
    let centres: Vec<Vec<f32>> = (0..clusters)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect();
    (0..n)
        .map(|i| {
            let v: Vec<f32> = centres[i % clusters].iter().map(|c| c + 0.6 * next()).collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.into_iter().map(|x| x / norm).collect()
        })
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn exact_top10(vs: &[Vec<f32>], q: &[f32]) -> Vec<usize> {
    let mut s: Vec<(usize, f32)> = vs.par_iter().enumerate().map(|(i, v)| (i, dot(v, q))).collect();
    s.sort_by(|a, b| b.1.total_cmp(&a.1));
    s.into_iter().take(10).map(|(i, _)| i).collect()
}

/// Recall@10 of a 40-candidate shortlist rescored with exact vectors, as the engine does.
fn recall(index: &dyn VectorIndex, vs: &[Vec<f32>], queries: &[Vec<f32>], truth: &[Vec<usize>]) -> f32 {
    let mut hits = 0;
    for (q, t) in queries.iter().zip(truth) {
        let mut short: Vec<(usize, f32)> = index
            .search(q, 40)
            .into_iter()
            .map(|(id, _)| {
                let i: usize = id[1..].parse().unwrap();
                (i, dot(&vs[i], q))
            })
            .collect();
        short.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits += short.iter().take(10).filter(|(i, _)| t.contains(i)).count();
    }
    hits as f32 / (queries.len() * 10) as f32
}

/// Median latency of one reader, and of each query with 8 readers running at once.
fn latency(index: &(dyn VectorIndex + Sync), queries: &[Vec<f32>]) -> (Duration, Duration) {
    let mut single: Vec<Duration> = queries
        .iter()
        .map(|q| {
            let t = Instant::now();
            index.search(q, 40);
            t.elapsed()
        })
        .collect();
    single.sort();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
    let mut concurrent: Vec<Duration> = pool.install(|| {
        queries
            .par_iter()
            .map(|q| {
                let t = Instant::now();
                index.search(q, 40);
                t.elapsed()
            })
            .collect()
    });
    concurrent.sort();
    (single[single.len() / 2], concurrent[concurrent.len() / 2])
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dim: usize = args.next().map_or(768, |a| a.parse().unwrap());
    let sizes: Vec<usize> = args.map(|a| a.parse().unwrap()).collect();
    let sizes = if sizes.is_empty() {
        vec![50_000, 100_000, 200_000]
    } else {
        sizes
    };
    let queries = clustered(200, dim, 500, 777);
    println!(
        "| vectors | index | build | p50 1 reader | p50 8 readers | recall@10 |\n|---|---|---|---|---|---|"
    );
    for n in sizes {
        let vs = clustered(n, dim, 500, 42);
        let truth: Vec<Vec<usize>> = queries.iter().map(|q| exact_top10(&vs, q)).collect();
        let t = Instant::now();
        let mut flat = FlatIndex::new();
        for (i, v) in vs.iter().enumerate() {
            flat.upsert(&format!("c{i}"), "t", v);
        }
        let flat_build = t.elapsed();
        let (s, c) = latency(&flat, &queries);
        let r = recall(&flat, &vs, &queries, &truth);
        println!("| {n} | flat | {flat_build:.1?} | {s:.2?} | {c:.2?} | {r:.3} |");
        if std::env::var_os("BENCH_FLAT_ONLY").is_some() {
            continue;
        }
        let t = Instant::now();
        let mut hnsw = HnswIndex::with_flat(flat);
        hnsw.rebuild();
        let build = t.elapsed();
        for ef in [64, 128, 200] {
            hnsw.set_ef_search(ef);
            let (s, c) = latency(&hnsw, &queries);
            let r = recall(&hnsw, &vs, &queries, &truth);
            println!("| {n} | hnsw ef={ef} | {build:.1?} | {s:.2?} | {c:.2?} | {r:.3} |");
        }
    }
}
