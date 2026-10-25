//! The contextual retrieval experiment (DRAG-48): chunks are exported per file for an
//! external model to situate, and its contexts are read back to index with the chunks.
//!
//! ```text
//! loom-eval export-chunks <root> --index-dir DIR --out chunks.jsonl
//! loom-eval quality <root> <golden…> --index-dir FRESH_DIR --contexts contexts.json
//! ```

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use loom_rag::engine::Engine;
use serde::Serialize;

/// One file and its chunks, a line of the export.
#[derive(Serialize)]
struct FileChunks {
    path: String,
    chunks: Vec<ChunkRef>,
}

#[derive(Serialize)]
struct ChunkRef {
    id: String,
    start_line: usize,
    end_line: usize,
    content: String,
}

/// Writes every indexed file with its chunks, one JSON object per line, files in path order.
pub fn export(engine: &Engine, out: &Path) -> anyhow::Result<usize> {
    let mut files: BTreeMap<String, Vec<ChunkRef>> = BTreeMap::new();
    engine.ledger().for_each_chunk(&mut |c| {
        files.entry(c.path.clone()).or_default().push(ChunkRef {
            id: c.id,
            start_line: c.start_line,
            end_line: c.end_line,
            content: c.content,
        });
    })?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(out)?);
    for (path, mut chunks) in files {
        chunks.sort_by_key(|c| c.start_line);
        serde_json::to_writer(&mut w, &FileChunks { path, chunks })?;
        w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(engine.status()?.files)
}

/// Reads `{chunk_id: context}`.
pub fn load(path: &Path) -> anyhow::Result<HashMap<String, String>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}
