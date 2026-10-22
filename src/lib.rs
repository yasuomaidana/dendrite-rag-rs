//! Retrieval engine for source-code repositories.
//!
//! See `docs/ARCHITECTURE.md` for the pipeline and `docs/specs/` for each stage.

pub mod chunk;
pub mod embed;
pub mod engine;
pub mod fusion;
pub mod graph;
pub mod history;
pub mod lang;
pub mod ledger;
pub mod lexical;
pub mod llm;
pub mod lookup;
pub mod rerank;
pub mod retrieve;
pub mod rpc;
pub mod server;
pub mod settings;
pub mod store;
pub mod symbols;
#[cfg(test)]
pub(crate) mod testutil;
pub mod vector;
pub mod walk;

/// Name reported by the `health` handshake.
pub const NAME: &str = "dendrite-rag";
/// Crate version reported by the `health` handshake.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// JSON-RPC protocol version; clients refuse a server with a different major version.
pub const PROTOCOL: &str = "1.2.0";
