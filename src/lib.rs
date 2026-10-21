//! Retrieval engine for source-code repositories.
//!
//! See `docs/ARCHITECTURE.md` for the pipeline and `docs/specs/` for each stage.

pub mod chunk;
pub mod lang;
pub mod rpc;
pub mod walk;

/// Name reported by the `health` handshake.
pub const NAME: &str = "dendrite-rag";
/// Crate version reported by the `health` handshake.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// JSON-RPC protocol version; clients refuse a server with a different major version.
pub const PROTOCOL: &str = "1.0.0";
