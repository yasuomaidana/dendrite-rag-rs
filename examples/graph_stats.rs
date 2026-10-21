//! Indexes a project (lexical only) and prints code-graph counts, timing and the symbols
//! with the most users.
//!
//!     cargo run --release --example graph_stats -- <root> [index-dir]

use std::time::Instant;

use dendrite_rag::engine::{Config, Engine};
use dendrite_rag::graph::EdgeKind;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".into());
    let mut cfg = Config::default();
    if let Some(dir) = args.next() {
        cfg.index_dir = Some(dir.into());
    }
    let mut engine = Engine::open_with_embedder(root.as_ref(), cfg, None)?;
    engine.index(false)?;
    let t = Instant::now();
    let g = engine.code_graph()?;
    let built = t.elapsed();
    let count = |kind: EdgeKind| {
        (0..g.file_count() as u32)
            .flat_map(|f| g.file_edges(f))
            .filter(|(_, k, _)| *k == kind)
            .count()
    };
    let (call, import, ty) = (
        count(EdgeKind::Call),
        count(EdgeKind::Import),
        count(EdgeKind::Type),
    );
    println!(
        "files {} symbols {} file edges: call {call} import {import} type {ty}; built in {built:?}",
        g.file_count(),
        g.symbol_count()
    );
    let mut top: Vec<u32> = (0..g.symbol_count() as u32).collect();
    top.sort_by_key(|&s| std::cmp::Reverse(g.in_degree(s)));
    for &s in top.iter().take(15) {
        let site = g
            .sites(s)
            .first()
            .map_or(String::new(), |x| format!("{}:{}", x.path, x.line));
        println!(
            "{:>4} in {:>4} out  {} {site}",
            g.in_degree(s),
            g.out_degree(s),
            g.symbol_name(s)
        );
    }
    Ok(())
}
