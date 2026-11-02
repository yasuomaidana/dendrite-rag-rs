# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-02

### Added
- **Syntax-Tree Chunking (cAST)**: Tree-sitter split-and-merge chunking across Go, Rust, Python, TypeScript, TSX, Java, C, and C++, preserving scope chains, signatures, and doc comments (`[DRAG-4]`).
- **Markdown & Text Chunking**: Hierarchical section splitter for Markdown headings and fallback line chunker for plain text (`[DRAG-4]`).
- **Code-Aware BM25**: Tantivy integration with custom `CodeTokenizer` that indexes identifiers as whole tokens and sub-tokens split across case transitions and digits (`[DRAG-4]`).
- **Configurable BM25 Parameters**: Added configurable Okapi BM25 `k1`, `b`, and Tsallis `q`-log IDF formula (`[lexical]` settings) shared by Tantivy, PostgreSQL, and `rank` (`[DRAG-47]`).
- **Pluggable Vector Embedders**: In-process Candle safetensors models (BERT, JinaBERT, NomicBERT with Metal/CUDA/Accelerate), OxiLLaMa GGUF runtime, Model2Vec static embeddings, and remote HTTP gateways (Ollama, TEI, OpenAI-compatible) (`[DRAG-18]`, `[DRAG-21]`).
- **Int8 Quantized Scan & HNSW**: Dot-product int8 flat vector scan with exact f32 candidate rescoring, and optional HNSW vector graph index behind feature `hnsw` (`[DRAG-37]`, `[DRAG-41]`).
- **Code Graph & Leiden Clustering**: Typed symbol graph extracting call, import, and type edges, and clustering files into semantic communities via the Leiden algorithm (`[DRAG-4]`, `[DRAG-31]`).
- **Weighted Reciprocal Rank Fusion**: Unified rank fusion (RRF, `k = 60.0`) over lexical BM25, dense vectors, and symbol matches with per-model default weights (`[DRAG-4]`, `[DRAG-26]`).
- **Token-Budgeted Context Packing**: Context pack assembly fitting target token budgets, with duplicate span folding, symbol graph neighbor expansion, and outline maps (`detail: map`) (`[DRAG-4]`, `[DRAG-51]`).
- **Local Embedded Ledgers**: Pure-Rust embedded key-value storage with redb (default), and opt-in SQLite ledger behind feature `sqlite` (`[DRAG-49]`).
- **PostgreSQL 16 + pgvector Tier**: Shared storage tier behind feature `postgres` (`[DRAG-33]`), with write-ahead edge synchronization journal (`[DRAG-34]`) and delta push with compaction (`[DRAG-35]`).
- **Incremental Change Detection**: BLAKE3 Merkle tree diffing (`embedag-core`) detecting changed files and reusing chunk embeddings by content hash (`[DRAG-4]`).
- **Git History Search**: Full-text indexing and search of commit messages and per-file diffs (`[DRAG-1]`).
- **Quality Gates Engine Module**: Implemented code quality evaluation in `loom_rag::quality` covering production lines of code, clone duplication, cyclomatic/cognitive complexity, package structure, and rstest conventions.

### Changed
- **Memory Footprint Optimization**: Batched cold file chunking and commits into 512-file groups, streaming ledger data directly into the code graph builder to cap 49k-chunk peak memory footprint at ~180-200 MB (`[DRAG-44]`).
- **Context Pack Documentation Share**: Configured mixed context packs to reserve a portion of the token budget (default 20%) for documentation sections (`[DRAG-38]`).
- **Test Parameterization**: Converted repetitive unit tests to parameterized cases using `rstest = "0.28.0"` (`[DRAG-63]`).

### Fixed
- **Retrieval Regression Removal**: Reverted unmeasured experimental retrieval changes (ALL_CAPS constants, adaptive fusion weights, and corpus auto routing) that caused regressions on golden benchmarks (`[DRAG-61]`).
- **SQLite Test File Length**: Split SQLite engine tests into a separate test file to comply with the 500-line file length limit (`[DRAG-49]`).
