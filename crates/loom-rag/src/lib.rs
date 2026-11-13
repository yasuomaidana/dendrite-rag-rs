//! Retrieval engine for code and documentation: syntax-aware chunking, code-aware BM25,
//! dense vectors, a symbol graph, weighted rank fusion and token-budgeted context packs.
//!
//! # Quick start
//!
//! ```no_run
//! use loom_rag::engine::{Config, Engine};
//! use loom_rag::retrieve::{Mode, SearchOpts};
//!
//! # fn main() -> anyhow::Result<()> {
//! let mut engine = Engine::open(std::path::Path::new("."), Config::default())?;
//! engine.index(false)?; // incremental: unchanged files are skipped
//!
//! let hits = engine.search("reciprocal rank fusion", &SearchOpts {
//!     top_k: 5,
//!     mode: Mode::Hybrid,
//!     ..SearchOpts::default()
//! })?;
//! let pack = engine.context("how does code chunking work?", 2000)?;
//! # let _ = (hits, pack);
//! # Ok(())
//! # }
//! ```
//!
//! # Modules
//!
//! - [`chunk`]: cAST chunking over tree-sitter syntax trees, plus Markdown and text splitters.
//! - [`engine`]: opens a project's index, keeps it current and answers queries.
//! - [`retrieve`]: hybrid search, rank fusion and token-budgeted context packs.
//! - [`lexical`]: Tantivy BM25 with a tokenizer that splits `camelCase` and `snake_case`.
//! - [`vector`]: int8-quantized exact scan, and HNSW behind the `hnsw` feature.
//! - [`embed`]: embedding backends (Ollama, TEI, OpenAI-compatible, candle, model2vec).
//! - [`graph`]: the typed code graph, communities and symbol ranking.
//! - [`ledger`] and [`store`]: the redb ledger, the optional SQLite ledger and the PostgreSQL tier.
//! - [`quality`]: code-quality checks (lines, clones, complexity, structure, rstest).
//!
//! # Feature flags
//!
//! Nothing is enabled by default. `hnsw`, `sqlite`, `postgres`, `model2vec`, `candle`
//! (`candle-metal`, `candle-cuda`, `candle-accelerate`) and `oxillama` add optional backends;
//! `testutil` exposes the test fixtures.
//!
//! The design notes and per-stage specifications are in the repository's `docs/` directory:
//! <https://github.com/yasuomaidana/dendrite-rag-rs/tree/main/docs>.

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
