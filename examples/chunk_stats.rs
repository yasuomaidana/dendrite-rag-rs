//! Walks and chunks a project, printing counts, timing and the largest chunks.
//!
//!     cargo run --release --example chunk_stats -- <root>

use std::time::Instant;

use dendrite_rag::chunk::{ChunkOptions, chunk_file};
use dendrite_rag::walk::{WalkOptions, read_text, walk};
use rayon::prelude::*;

fn main() -> anyhow::Result<()> {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let t = Instant::now();
    let files = walk(root.as_ref(), &WalkOptions::default())?;
    let walked = t.elapsed();
    let chunks: Vec<_> = files
        .par_iter()
        .flat_map(|f| {
            read_text(&f.abs).map_or_else(Vec::new, |src| chunk_file(&f.path, f.lang, &src, ChunkOptions::default()))
        })
        .collect();
    let total = t.elapsed();
    let with_scope = chunks.iter().filter(|c| !c.scope.is_empty()).count();
    let with_symbols = chunks.iter().filter(|c| !c.symbols.is_empty()).count();
    println!("files {} chunks {} walk {walked:?} total {total:?}", files.len(), chunks.len());
    println!("chunks with scope {with_scope}, with symbols {with_symbols}");
    let mut by_size: Vec<_> = chunks.iter().map(|c| (c.content.len(), &c.path, c.start_line, c.end_line)).collect();
    by_size.sort();
    let n = by_size.len();
    if n > 0 {
        println!("median bytes {} p95 {} max {:?}", by_size[n / 2].0, by_size[n * 95 / 100].0, by_size[n - 1]);
    }
    Ok(())
}
