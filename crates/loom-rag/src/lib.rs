//! Retrieval engine for code and documentation: syntax-aware chunking, code-aware BM25,
//! dense vectors, a symbol graph, weighted rank fusion and token-budgeted context packs.
//!
//! See `docs/ARCHITECTURE.md` for the pipeline and `docs/specs/` for each stage.

pub mod chunk;
pub mod embed;
pub mod engine;
pub mod graph;
pub mod ledger;
pub mod lexical;
pub mod quality;
pub mod retrieve;
pub mod store;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
pub mod vector;

pub use chunk::{lang, walk};
pub use engine::{config, corpus, llm};
pub use graph::symbols;
pub use ledger::history;
pub use lexical::bm25;
pub use retrieve::{fusion, lookup, rank, rerank};

/// Compiles the Rust examples of the crate README as doctests, so they cannot drift from the code.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
