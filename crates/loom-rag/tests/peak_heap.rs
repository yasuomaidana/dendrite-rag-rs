//! Peak heap of a cold index, under dhat (`--features dhat,testutil`): the memory gate of
//! `docs/specs/retrieval.md` § Performance targets, checked without a large checkout.
//!
//!     cargo test -p loom-rag --features dhat,testutil --test peak_heap -- --nocapture

#![cfg(all(feature = "dhat", feature = "testutil"))]

use loom_rag::engine::{Config, Engine};
use loom_rag::testutil::synthetic_project;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// 4,000 small files (one chunk each). dhat makes a run slow, so this is a regression
/// gate; the 200 MB budget itself is measured at 49k chunks (`eval/RESULTS.md` § DRAG-44).
const FILES: usize = 4_000;
/// Measured 61 MB (2026-10-24), most of it fixed: Tantivy's 50 MB writer budget.
const PEAK_BYTES: usize = 96 << 20;

#[test]
fn cold_index_peak_heap_stays_within_budget() {
    let dir = synthetic_project(FILES);
    let _profiler = dhat::Profiler::builder().testing().build();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
    let r = e.index(false).unwrap();
    let stats = dhat::HeapStats::get();
    println!(
        "cold index: {} files, {} chunks; peak heap {:.1} MB in {} blocks; {} allocations in total",
        r.files_scanned,
        r.chunks_added,
        stats.max_bytes as f64 / (1 << 20) as f64,
        stats.max_blocks,
        stats.total_blocks
    );
    dhat::assert!(stats.max_bytes < PEAK_BYTES);
}
