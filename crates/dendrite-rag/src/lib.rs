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
pub const PROTOCOL: &str = "1.8.0";
