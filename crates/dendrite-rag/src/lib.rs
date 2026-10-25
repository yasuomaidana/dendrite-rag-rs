//! Dendrite's retrieval sidecar on the loom-rag engine: the JSON-RPC protocol
//! (`docs/specs/protocol.md`), the `.dendrite/rag.toml` settings and their `DENDRITE_RAG_*`
//! environment overrides, and Dendrite's `ask` prompts.

pub mod ask;
pub mod rpc;
pub mod server;
pub mod settings;

/// Name reported by the `health` handshake.
pub const NAME: &str = "dendrite-rag";
/// Crate version reported by the `health` handshake.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// JSON-RPC protocol version; clients refuse a server with a different major version.
pub const PROTOCOL: &str = "1.9.0";

/// The cargo features this binary was built with, so a client can tell, before any
/// project is opened, what `[embed]`, `[storage]` and `[rerank]` settings it can honour.
pub fn features() -> Vec<&'static str> {
    [
        (cfg!(feature = "postgres"), "postgres"),
        (cfg!(feature = "sqlite"), "sqlite"),
        (cfg!(feature = "hnsw"), "hnsw"),
        (cfg!(feature = "model2vec"), "model2vec"),
        (cfg!(feature = "candle"), "candle"),
        (cfg!(feature = "candle-metal"), "candle-metal"),
        (cfg!(feature = "candle-cuda"), "candle-cuda"),
        (cfg!(feature = "candle-accelerate"), "candle-accelerate"),
        (cfg!(feature = "oxillama"), "oxillama"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}
