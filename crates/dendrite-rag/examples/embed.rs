//! Prints the document vectors of the configured embedder for each argument, as JSON.
//! Used to compare backends and implementations:
//! `DENDRITE_RAG_EMBED_BACKEND=candle cargo run --release --features candle --example embed -- "text" …`

use dendrite_rag::settings::Settings;
use loom_rag::embed::{self, Embedder};

fn main() -> anyhow::Result<()> {
    let texts: Vec<String> = std::env::args().skip(1).collect();
    let settings = Settings::load(std::path::Path::new("."))?;
    let embedder: Box<dyn Embedder> =
        embed::from_config(&settings.embed)?.ok_or_else(|| anyhow::anyhow!("backend is none"))?;
    let vectors = embedder.embed_documents(&texts)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({ "model": embedder.model_id(), "vectors": vectors }))?
    );
    Ok(())
}
