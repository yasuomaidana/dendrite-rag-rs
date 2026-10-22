//! Retrieval engine for code and documentation: syntax-aware chunking, code-aware BM25,
//! dense vectors, a symbol graph, weighted rank fusion and token-budgeted context packs.
//!
//! See `docs/ARCHITECTURE.md` for the pipeline and `docs/specs/` for each stage.

pub mod chunk;
pub mod config;
pub mod corpus;
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
pub mod rank;
pub mod rerank;
pub mod retrieve;
pub mod store;
pub mod symbols;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
pub mod vector;
pub mod walk;
