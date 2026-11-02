# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- CI: `.github/workflows/complexity.yml` cross-checks function complexity with clippy `cognitive_complexity`, `lizard` and `ruff` C901; the Rust steps are report only while DRAG-78 is open (DRAG-82).

## [0.1.0] - 2026-11-02

### Added
- **Workspace Architecture & Extraction**: Scaffolded the `dendrite-rag-rs` Cargo workspace extracting hybrid retrieval, storage, and evaluation from Dendrite (`[DEN-974]`, `[DRAG-1]`).
- **Core Retrieval Engine (`loom-rag`)**:
  - Tree-sitter syntax-aware chunking (cAST) across Go, Rust, Python, TypeScript, TSX, Java, C, and C++ (`[DRAG-4]`).
  - Code-aware Okapi BM25 full-text search via Tantivy with `CodeTokenizer` splitting on camelCase, snake_case, and digits (`[DRAG-4]`).
  - Pluggable vector embeddings supporting Candle (Metal/CUDA/CPU), OxiLLaMa GGUF runtime, Model2Vec, Ollama, Hugging Face TEI, and OpenAI gateways (`[DRAG-18]`, `[DRAG-21]`).
  - Int8 quantized flat vector scan with exact float rescoring, and optional HNSW graph indexing behind feature `hnsw` (`[DRAG-37]`, `[DRAG-41]`).
  - Typed symbol graph with call, import, and type edges, plus Leiden community clustering (`[DRAG-4]`, `[DRAG-31]`).
  - Weighted Reciprocal Rank Fusion (RRF, `k = 60.0`) fusing lexical, dense, and symbol rankings (`[DRAG-4]`, `[DRAG-26]`).
  - Token-budgeted context packing with duplicate text folding, graph neighbor expansion, and outline map format (`[DRAG-4]`, `[DRAG-51]`).
  - Local-first embedded storage using redb by default, with opt-in SQLite ledger behind feature `sqlite` (`[DRAG-49]`).
  - PostgreSQL 16 + pgvector shared tier behind feature `postgres` (`[DRAG-33]`), with write-ahead edge synchronization (`[DRAG-34]`) and delta push with compaction (`[DRAG-35]`).
  - Incremental change detection via BLAKE3 Merkle tree diffing (`embedag-core`) (`[DRAG-4]`).
  - Git history commit message and diff search (`[DRAG-1]`).
  - Code quality analysis module (`loom_rag::quality`) supporting line counting, clones, complexity, structure, and rstest checks.
- **Sidecar Daemon & CLI (`dendrite-rag`)**:
  - JSON-RPC 2.0 stdio server (`dendrite-rag serve`) and developer CLI commands (`[DRAG-1]`).
  - Project configuration via `.dendrite/rag.toml` and hierarchical `DENDRITE_RAG_*` environment overrides (`[DRAG-1]`).
  - Protocol evolutions through 1.11.0: `graph.*` methods, `rank` document ranking, multi-corpus search, stage timings, `docs_share` budget allocation, `abstained` flag, token budgets, and `quality.*` RPC methods (`[DRAG-56]`, `[DRAG-60]`).
  - Multi-tier `ask` question-answering workflow with prompt generation, local model drafting, hybrid refinement, and ledger answer caching (`[DEND-203]`, `[DRAG-4]`, `[DRAG-38]`).
  - Idle engine closing and lock wait handling (`[DEN-348]`).
  - Configurable answer token cap (`[DEN-349]`).
- **Quality Gates Binary (`loom-gate`)**:
  - CLI binary evaluating source trees against `.github/dendrite/directives.toml`.
  - Subcommands: `loc` (production lines of code), `clones` (syntax duplication), `complexity` (McCabe/SonarSource limits), `structure` (directory modularity), and `rstest` (Rust test conventions).
  - Deterministic exit codes (0 = pass, 1 = fail, 2 = usage error) and output formats (`text`, `json`, `github`).
- **Evaluation Suite (`loom-eval`)**:
  - Evaluation harness measuring Hit@k, MRR@10, nDCG@10, and token economics (recall@1k/2.5k/4k) against baselines (`grep-agent`, `bm25-file`, `naive-chunks`).
  - Paired bootstrap hypothesis testing (`loom-eval compare`) with 95% confidence intervals.
  - SWE-bench Verified file localization runner (`loom-eval swebench`) with Stratified-120 subset and leakage classes (`[DRAG-54]`, `[DRAG-55]`).
  - Rerank abstention probe (`loom-eval abstain-probe`) and cold/warm repeat timing (`loom-eval repeat`).
- **Quality gates**: `loom-gate` with `loc`, `clones`, `rstest`, `complexity` (cyclomatic and cognitive, measured by dendrite-core-rs) and `structure` (modules per directory, lines per directory, name-prefix groups, and a `docs` rule requiring a README and a CHANGELOG for every crate and project), served as `quality.*` methods in protocol 1.12.0 (`[DRAG-75]`, `[DRAG-76]`, `[DRAG-81]`).

### Changed
- **Memory Footprint Optimization**: Committed cold indexing in 512-file batches and streamed the ledger into the code graph, reducing peak footprint at 49k chunks from 556 MB to ~180-200 MB (`[DRAG-44]`).
- **Lexical BM25 Tuning**: Made BM25 `k1`, `b`, and Tsallis `q`-log IDF configurable across Tantivy, PostgreSQL, and document ranking (`[DRAG-47]`).
- **Test Parameterization**: Parameterized repetitive test cases across the workspace with `rstest = "0.28.0"` (`[DRAG-63]`).
- **Documentation**: Audited crate READMEs and documentation against source implementations (`[DRAG-53]`, `[DRAG-59]`).

### Fixed
- **Retrieval Regression Removal**: Reverted unmeasured experimental retrieval changes (ALL_CAPS constants, adaptive fusion weights, and corpus auto routing) that caused regressions in golden benchmark evaluations (`[DRAG-61]`).
- **Test Length Limits**: Split SQLite storage engine tests into a separate test file to conform to 500-line limit (`[DRAG-49]`).
