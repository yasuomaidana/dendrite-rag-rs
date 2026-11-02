# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-02

### Added
- **JSON-RPC 2.0 Daemon**: Line-delimited JSON-RPC 2.0 server over stdio (`dendrite-rag serve`) for integration with `dendrite-control` Go sidecar clients and `dendrite-mcp-hub` (`[DEN-974]`, `[DRAG-1]`).
- **Command-Line Interface**: Terminal subcommands for `index`, `search`, `context`, `show`, `symbols`, `grep`, `history`, `graph`, `status`, `settings`, `embedders`, `purge`, `sync`, `push`, and `pull` (`[DRAG-1]`).
- **Project Settings Loader**: Hierarchical configuration parser reading `<root>/.dendrite/rag.toml` with `DENDRITE_RAG_*` environment variable overrides (`[DRAG-1]`).
- **Question-Answering Cascade (`ask`)**:
  - Retrieval-augmented answering workflow generating prompt context and invoking models through the LiteLLM gateway (`[DEND-203]`, `[DRAG-4]`).
  - Multi-tier model execution: local model drafting (`local-coder`), hybrid refinement (`gemini-2.5-pro`), and paid escalation requiring explicit caller consent (`allow_paid`).
  - Answer caching in the project's local ledger keyed by model and exact prompt text (`[DRAG-4]`).
  - Context budget documentation reservation via `docs_share` (protocol 1.7) (`[DRAG-38]`).
  - Reranker relevance abstention reporting (`abstained: true`, protocol 1.8) (`[DRAG-51]`).
- **Protocol Increments**:
  - Protocol 1.1: Added `graph.*` methods and community statistics.
  - Protocol 1.2: Added `sync`, `pull`, and `push` shared storage synchronization.
  - Protocol 1.3: Added `rank` method for hybrid document ranking without an index.
  - Protocol 1.4: Added `ask` method.
  - Protocol 1.5: Added `corpus` parameter and retrieval stage timings.
  - Protocol 1.6: Added `context.detail`, `chunks.get`, `index.focus`, `duplicates`, and `ask.fresh`.
  - Protocol 1.7: Added `docs_share` to `context` and `ask`.
  - Protocol 1.8: Added `abstained` flag in `context` and `ask` results.
  - Protocol 1.9: Added `embed_budget_tokens` index parameter, `embed_tokens` result reporting, and `features` list in `health` (`[DRAG-56]`, `[DRAG-60]`).
  - Protocol 1.10: Added `quality.loc` RPC method for production lines of code verification.
  - Protocol 1.11: Added `quality.clones`, `quality.complexity`, `quality.structure`, and `quality.rstest` RPC methods.

### Changed
- **Server Concurrency & Lifecycle**: Automatically close idle project engines after a timeout to release the index lock, and wait for held locks rather than immediately failing (`[DEN-348]`).
- **Embedder Cache Across Reopens**: Reused loaded embedder models across project reopens within the same process (`[DEN-348]`).
- **Answer Length Limits**: Capped generated `ask` response tokens (default 600 tokens, configurable via `DENDRITE_RAG_ASK_MAX_TOKENS`) to prevent unbounded generation latency (`[DEN-349]`).
- **Documentation**: Audited crate README and protocol specifications against implementation (`[DRAG-53]`, `[DRAG-59]`).

### Fixed
- **Failed Embedding Counting**: Ensured failed embedding attempts correctly count as pending vectors for subsequent index runs (`[DRAG-56]`, `[DRAG-60]`).
