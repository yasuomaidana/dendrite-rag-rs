//! Retrieval quality and performance report for a golden query set (DRAG-15).
//!
//!     cargo run --release --example eval -- <root> eval/golden-dendrite.json [index-dir]
//!
//! Embeddings follow the usual environment (`DENDRITE_RAG_EMBED_BACKEND`, `..._MODEL`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dendrite_rag::engine::{Config, Engine, cache_key};
use dendrite_rag::retrieve::{Mode, SearchOpts};
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Query {
    q: String,
    expect: Vec<String>,
}

#[derive(Default)]
struct Scores {
    hit1: usize,
    hit5: usize,
    hit10: usize,
    rr: f32,
    misses: Vec<String>,
}

fn rank(engine: &Engine, q: &Query, mode: Mode) -> anyhow::Result<Option<usize>> {
    let hits = engine.search(
        &q.q,
        &SearchOpts {
            top_k: 10,
            mode,
            ..SearchOpts::default()
        },
    )?;
    Ok(hits
        .iter()
        .position(|h| q.expect.iter().any(|e| e == &h.path))
        .map(|p| p + 1))
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn max_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    // macOS reports bytes, Linux kilobytes.
    let bytes = if cfg!(target_os = "macos") {
        u.ru_maxrss as f64
    } else {
        u.ru_maxrss as f64 * 1024.0
    };
    bytes / (1024.0 * 1024.0)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = args.first().map(String::as_str).unwrap_or(".");
    let golden: Golden = serde_json::from_slice(&std::fs::read(
        args.get(1)
            .map(String::as_str)
            .unwrap_or("eval/golden-dendrite.json"),
    )?)?;
    let mut cfg = Config::resolve(root.as_ref())?;
    cfg.index_dir = args.get(2).map(Into::into);
    eprintln!("rss start: {:.0} MB", max_rss_mb());
    let mut engine = Engine::open(root.as_ref(), cfg)?;
    eprintln!("rss after open: {:.0} MB", max_rss_mb());
    let report = engine.index(false)?;
    eprintln!("rss after index: {:.0} MB", max_rss_mb());
    engine.symbol_index()?;
    eprintln!("rss after symbol index: {:.0} MB", max_rss_mb());
    let status = engine.status()?;
    println!("index: {report:?}");
    println!(
        "corpus: {} files, {} chunks, {} vectors, model {:?}\n",
        status.files, status.chunks, status.vectors, status.embed_model
    );

    // Quality per mode.
    let mut modes = vec![Mode::Lexical, Mode::Hybrid];
    if engine.has_vectors() {
        modes.insert(1, Mode::Dense);
    }
    println!("| mode | hit@1 | hit@5 | hit@10 | MRR@10 |\n|---|---|---|---|---|");
    let n = golden.queries.len() as f32;
    let mut hybrid_misses = Vec::new();
    for mode in &modes {
        let mut s = Scores::default();
        for q in &golden.queries {
            match rank(&engine, q, *mode)? {
                Some(r) => {
                    s.hit1 += (r == 1) as usize;
                    s.hit5 += (r <= 5) as usize;
                    s.hit10 += 1;
                    s.rr += 1.0 / r as f32;
                }
                None => s.misses.push(q.q.clone()),
            }
        }
        println!(
            "| {mode:?} | {:.2} | {:.2} | {:.2} | {:.3} |",
            s.hit1 as f32 / n,
            s.hit5 as f32 / n,
            s.hit10 as f32 / n,
            s.rr / n
        );
        if *mode == Mode::Hybrid {
            hybrid_misses = s.misses;
        }
    }
    if !hybrid_misses.is_empty() {
        println!("\nhybrid misses: {hybrid_misses:#?}");
    }

    eprintln!("rss after quality: {:.0} MB", max_rss_mb());
    // Latency with 8 concurrent readers. Query embeddings are computed once up front, so
    // the numbers measure retrieval, not the embedding model.
    let queries: Vec<&str> = golden.queries.iter().map(|q| q.q.as_str()).collect();
    let embedded: Vec<Option<Vec<f32>>> = queries
        .iter()
        .map(|q| engine.embed_query(q).ok().flatten())
        .collect();
    let rounds = 10;
    let mut lexical = Vec::new();
    let mut vector = Vec::new();
    let engine_ref = &engine;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let queries = &queries;
                let embedded = &embedded;
                scope.spawn(move || {
                    let mut lex = Vec::new();
                    let mut vec = Vec::new();
                    for r in 0..rounds {
                        for (i, q) in queries.iter().enumerate() {
                            if (i + t + r) % 2 == 0 {
                                let start = Instant::now();
                                let _ = engine_ref.search(
                                    q,
                                    &SearchOpts {
                                        mode: Mode::Lexical,
                                        ..SearchOpts::default()
                                    },
                                );
                                lex.push(start.elapsed());
                            }
                            if let Some(v) = &embedded[i] {
                                let start = Instant::now();
                                let _ = engine_ref.search_vector(v, 50);
                                vec.push(start.elapsed());
                            }
                        }
                    }
                    (lex, vec)
                })
            })
            .collect();
        for h in handles {
            let (l, v) = h.join().unwrap();
            lexical.extend(l);
            vector.extend(v);
        }
    });
    lexical.sort();
    vector.sort();
    println!(
        "\nlatency, 8 concurrent readers ({} lexical, {} vector searches):",
        lexical.len(),
        vector.len()
    );
    println!(
        "  lexical search (BM25 + ledger fetch): p50 {:?} p95 {:?}",
        percentile(&lexical, 0.5),
        percentile(&lexical, 0.95)
    );
    if !vector.is_empty() {
        println!(
            "  vector scan (top 50):                p50 {:?} p95 {:?}",
            percentile(&vector, 0.5),
            percentile(&vector, 0.95)
        );
    }

    println!(
        "max RSS after indexing and the latency run: {:.0} MB",
        max_rss_mb()
    );

    // Rescored int8 search against exact f32 search over the real cached vectors (this
    // check loads every vector, so it runs after the RSS measurement).
    if let Some(model) = &status.embed_model {
        let mut exact: Vec<(String, Vec<f32>)> = Vec::new();
        engine.ledger().for_each_chunk(|c| {
            if let Ok(Some(v)) = engine.ledger().embedding(&cache_key(model, &c.content_hash)) {
                exact.push((c.id, v));
            }
        })?;
        let sample: Vec<usize> = (0..exact.len())
            .step_by((exact.len() / 100).max(1))
            .take(100)
            .collect();
        let mut overlap = 0;
        for &i in &sample {
            let q = &exact[i].1;
            let mut scored: Vec<(&str, f32)> = exact
                .iter()
                .map(|(id, v)| (id.as_str(), v.iter().zip(q).map(|(a, b)| a * b).sum()))
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1));
            let truth: HashMap<&str, ()> = scored.iter().take(10).map(|(id, _)| (*id, ())).collect();
            overlap += engine
                .search_vector(q, 10)?
                .iter()
                .filter(|(id, _)| truth.contains_key(id.as_str()))
                .count();
        }
        println!(
            "\nvector search vs exact f32 recall@10 over {} real query vectors: {:.3}",
            sample.len(),
            overlap as f32 / (sample.len() * 10) as f32
        );
    }
    Ok(())
}
