# dendrite-rag-rs Documentation

This directory contains architectural boundary definitions, interface contracts, and reference documentation for `dendrite-rag-rs`.

## Repository Documentation

- [Responsibilities & Architecture Boundary](RESPONSIBILITIES.md) (`docs/RESPONSIBILITIES.md`) — Authoritative scope, owned vs. excluded capabilities, JSON-RPC 2.0 sidecar interface contract, Go packages being extracted, sibling repository dependencies, Plane v2 ticket map, and open boundary questions.
- [Imported Documentation Manifest](imports.json) (`docs/imports.json`) — Machine-readable list of upstream `Dendrite` control-plane documentation paths relevant to `dendrite-rag-rs`.

## Imported from Dendrite

<!-- Generated from imports.json by scripts/sync_subproject_docs.py (control-plane repo). The list is regenerated; descriptions are kept and must describe this repository. -->

Specifications that define `dendrite-rag-rs`'s responsibilities, listed in [`imports.json`](imports.json) and copied into `docs/imported/`. The first line of each copy records where it was copied from. The copies are regenerated, so do not edit them here. Each entry below says what the spec defines for this repository.

### Architecture

- [`2026-10-quality-audit-execution-plan`](imported/architecture/2026-10-quality-audit-execution-plan.md) — Boundary of dendrite-rag-rs in the five-repository split: the hybrid retrieval, content-addressed store and memory sync it owns.

### Feature specifications

- [`FEAT-022-agent-search-and-capability-discovery`](imported/features/FEAT-022-agent-search-and-capability-discovery.md) — Search primitives dendrite-rag-rs owns: BM25 and vector scoring fused with Reciprocal Rank Fusion.
- [`FEAT-031-persistent-store-hybrid-token-scheduler`](imported/features/FEAT-031-persistent-store-hybrid-token-scheduler.md) — Segmented persistent store dendrite-rag-rs implements: chunk checkpoints, partial queries and blob storage.
- [`FEAT-045-code-knowledge-mcp-graphify`](imported/features/FEAT-045-code-knowledge-mcp-graphify.md) — Code-knowledge retrieval dendrite-rag-rs provides over AST and graph chunks.

### CLI reference

- [`ask`](imported/cli/ask.md) — Ask command whose retrieval stage is dendrite-rag-rs.
- [`store`](imported/cli/store.md) — Store commands backed by dendrite-rag-rs's content-addressed store.

### MCP tool reference

- [`dendrite_rag_retrieve`](imported/mcp/dendrite_rag_retrieve.md) — Tool contract for hybrid BM25 + vector retrieval served by dendrite-rag-rs.
- [`dendrite_rag_ask`](imported/mcp/dendrite_rag_ask.md) — Tool contract for grounded answers; dendrite-rag-rs supplies the retrieved, cited context.
- [`dendrite_store_commit_chunk`](imported/mcp/dendrite_store_commit_chunk.md) — Tool contract for atomic chunk commits into dendrite-rag-rs's store.
- [`dendrite_store_inspect`](imported/mcp/dendrite_store_inspect.md) — Tool contract for chunk and segment inspection of a project in dendrite-rag-rs's store.
- [`dendrite_store_query_partial`](imported/mcp/dendrite_store_query_partial.md) — Tool contract for querying committed segments in dendrite-rag-rs, including partially analysed projects.
- [`dendrite_store_rm`](imported/mcp/dendrite_store_rm.md) — Tool contract for removing a project's records and unlinking its blobs in dendrite-rag-rs.
- [`dendrite_store_status`](imported/mcp/dendrite_store_status.md) — Tool contract for the store status and checkpoint state reported by dendrite-rag-rs.

### Guides

- [`asking-questions-to-dendrite`](imported/guides/asking-questions-to-dendrite.md) — End-user question flow whose retrieval step runs in dendrite-rag-rs: what is indexed and how results are ranked.

