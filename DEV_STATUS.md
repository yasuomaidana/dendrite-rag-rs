# DRAG Project Development Status & Roadmap

This document maintains the real-time development status, milestones, and active ticket execution pipeline for
**DRAG**, managed following the autonomous ticket lifecycle protocol.

---

## Current Status Overview

- **Current Focus**: `DRAG-087: Track DEV_STATUS.md generated from its own Plane project`
- **Last Updated**: 2026-11-03
- **Summary**: Backlog: 12 | To Do: 1 | In Progress: 7 | To Verify: 17 | Done: 50 (Total: 87)
- **Overall Progress**: 50 / 87 Tickets Completed (57.5%)

---

## Milestone Execution Pipeline

### M7: Go/Rust Decomposition

- [ ] **DRAG-1**: dendrite-rag-rs: BM25+vector hybrid retrieval and memory sync engine [State: Backlog, Priority: Medium]

### Milestone 26: Persistent Segmented Store &amp; Token-Aware Hybrid Scheduler

- [ ] **DRAG-6**: [DEN-059] BLAKE3 Content-Addressed Persistent Segmented Store & Chunk Checkpointer (`pkg/store/segmented`, `cmd/dendrite`) [State: To Verify, Priority: Medium]

### 18: Agent Search, Capability Discovery &amp; Semantic Routing Engine.

- [ ] **DRAG-7**: [DEN-037] Hybrid Agent Search, TEI Embeddings & Semantic Routing Engine (`pkg/agentsearch`, `cmd/dendrite`) [State: In Progress, Priority: High]

### Code RAG engine

- [x] **DRAG-9**: feat(crate): dendrite-rag crate and binary, proto/ schemas and JSON-RPC health [State: Done, Priority: High]
- [x] **DRAG-10**: feat(chunk): gitignore-aware walker, cAST tree-sitter chunker and contextual chunk headers [State: Done, Priority: High]
- [x] **DRAG-11**: feat(index): redb ledger, Merkle incremental indexing and code-aware Tantivy BM25 [State: Done, Priority: High]
- [x] **DRAG-12**: feat(embed): Embedder trait (LiteLLM gateway, model2vec), embedding cache, flat/HNSW vector index [State: Done, Priority: High]
- [x] **DRAG-13**: feat(search): weighted RRF hybrid search, symbol-graph expansion and token-budgeted context packing [State: Done, Priority: High]
- [x] **DRAG-14**: feat(rpc): serve index/search/context/status/purge over JSON-RPC and dev CLI [State: Done, Priority: High]
- [x] **DRAG-15**: test(eval): golden query set, recall@10/MRR report and criterion benchmark gate [State: Done, Priority: Medium]
- [x] **DRAG-16**: feat(history): searchable git history (commit messages and per-file diffs) as a retrieval source [State: Done, Priority: Medium]
- [x] **DRAG-17**: feat(index): time budget for the embedding phase so callers never block on a first full embedding [State: Done, Priority: High]

### General

- [ ] **DRAG-2**: [DEN-964] memory modes: local-only / edge (WAL) / push (+compaction) (was DEN #189) [State: Backlog, Priority: Medium]
- [ ] **DRAG-3**: [DEN-803] Structured Topic Memory Parser & Zero-Extrapolation Fidelity Guard (was DEN #131) [State: To Verify, Priority: Medium]
- [ ] **DRAG-4**: [DEND-203] [Local-First RAG & Shared Library Deduplication]: Zero-cost nomic embeddings & CAS library store (was DEN #108) [State: In Progress, Priority: Urgent]
- [ ] **DRAG-5**: [DEN-060] Store Inspection, Chunk Status Telemetry & Explicit User Purge Lifecycler (`pkg/store/status`, `pkg/ui/store`) [State: To Verify, Priority: Medium]
- [x] **DRAG-8**: docs: own technical specification (ARCHITECTURE.md + specs/), drop imported copies [State: Done, Priority: High]
- [x] **DRAG-18**: feat(embed): Ollama and Hugging Face Inference embedding backends; user-selected backend and model from .dendrite/rag.toml or env; embedders listing [State: Done, Priority: High]
- [x] **DRAG-19**: feat(embed): in-process candle embedder (BERT, NomicBERT, JinaBERT) with Metal/accelerate features [State: Done, Priority: High]
- [x] **DRAG-20**: feat(embed): in-process OxiLLaMa GGUF embedder (Qwen3-Embedding and other decoder embedders) [State: Done, Priority: High]
- [x] **DRAG-21**: feat(embed): auto backend prefers GPU: candle (Metal/CUDA), then a Hugging Face TEI endpoint, then Ollama; oxillama documented as CPU-only for embeddings; eval results for every backend [State: Done, Priority: High]
- [ ] **DRAG-57**: test(embed): paid code embedder evaluation (needs keys and consent) [State: To Do, Priority: Medium]
- [ ] **DRAG-67**: loom-gate rstest: use the rstest crate (0.28.0), not hand-written helpers [State: To Verify, Priority: High]
- [ ] **DRAG-68**: Rust quality pass 2: loom-gate rstest, clones and rstest dependency clean in dendrite-rag-rs [State: To Verify, Priority: High]
- [ ] **DRAG-69**: loom-gate rstest strict: helpers with parameters repeated by tests must be fixtures too [State: To Verify, Priority: Medium]
- [ ] **DRAG-70**: ask streams its answer: ask.chunk notifications (protocol 1.12.0) [State: Backlog, Priority: High]
- [ ] **DRAG-71**: Clone batch R1 (rag-rs): embed and vector [State: To Verify, Priority: High]
- [ ] **DRAG-72**: Clone batch R2 (rag-rs): stores, ledger and engine tests [State: To Verify, Priority: High]
- [ ] **DRAG-73**: rag-rs clone batch R3: symbols, chunk, graph, quality tests, loom-eval baselines [State: To Verify, Priority: Medium]
- [ ] **DRAG-74**: rag-rs clone batch R4: dendrite-rag settings/server and loom-eval runs [State: To Verify, Priority: Medium]
- [ ] **DRAG-75**: quality.complexity gate: cyclomatic and cognitive complexity per function (loom-gate, RPC) [State: To Verify, Priority: Medium]
- [ ] **DRAG-76**: structure quality gate: semantically grouped modules, not just files under 500 lines [State: In Progress, Priority: Medium]
- [ ] **DRAG-77**: Group rag-rs directories the structure gate flags: loom-rag/src (19 modules, 3585 lines), embed (14), quality (16) [State: Backlog, Priority: Medium]
- [ ] **DRAG-78**: Reduce the 78 functions over complexity 10/15 in rag-rs, then gate it in CI [State: Backlog, Priority: Medium]
- [ ] **DRAG-79**: Move agentsearch, store and the symbol graph (blast radius) from Go into dendrite-rag-rs [State: Backlog, Priority: Medium]
- [ ] **DRAG-80**: README and CHANGELOG for the project and every crate [State: To Verify, Priority: Medium]
- [ ] **DRAG-81**: structure gate: every crate, module and project has a README and a CHANGELOG (docs rule) [State: In Progress, Priority: Medium]
- [ ] **DRAG-82**: Second complexity check in CI with clippy cognitive_complexity, lizard and ruff [State: In Progress, Priority: Medium]
- [ ] **DRAG-83**: Compile the crate READMEs as doctests [State: To Verify, Priority: Medium]
- [ ] **DRAG-84**: complexity workflow: ruff is report-only while the eval scripts are over the limit [State: To Verify, Priority: Medium]
- [ ] **DRAG-85**: Adopt generated CI and clear the quality gate (78 complexity, 4 structure) [State: Backlog, Priority: Medium]
- [ ] **DRAG-086**: Untrack .dendrite local state [State: To Verify, Priority: Medium]
- [ ] **DRAG-087**: Track DEV_STATUS.md generated from its own Plane project [State: In Progress, Priority: Medium]

### Governance

- [x] **DRAG-64**: Governance 2.1: loom-gate LOC counter with test stripping [State: Done, Priority: High]
- [ ] **DRAG-65**: Governance 2.2: structural clone detector and rstest check [State: To Verify, Priority: High]
- [ ] **DRAG-66**: Governance 4.2: graph.symbols and edit.move_symbols [State: Backlog, Priority: Medium]

### Measured retrieval enhancements (Gemini review)

- [x] **DRAG-45**: test(eval): bootstrap confidence intervals and paired compare in loom-eval [State: Done, Priority: High]
- [x] **DRAG-46**: perf(bench): criterion benches and dhat peak-heap gate [State: Done, Priority: Medium]
- [x] **DRAG-47**: feat(lexical): tunable BM25 k1/b and q-log IDF in Tantivy, PostgreSQL and rank, with a sweep [State: Done, Priority: High]
- [x] **DRAG-48**: test(eval): go/no-go experiment for LLM contextual chunk prefixes on code [State: Done, Priority: Medium]
- [x] **DRAG-49**: feat(store): SQLite + sqlite-vec local ledger backend (opt-in), Turso rejected [State: Done, Priority: High]
- [ ] **DRAG-50**: feat(enrich): enrichment pipeline - blocked by the DRAG-58 re-run [State: Backlog, Priority: Medium]
- [x] **DRAG-51**: feat(retrieve): reranker-calibrated abstention and outline fallback in context packs [State: Done, Priority: Medium]
- [x] **DRAG-52**: perf(embed): Qwen3 candle embedder/reranker, paid code embedders and external eval subsets [State: Done, Priority: Low]
- [x] **DRAG-53**: docs(eval): DRAG-48 is inconclusive, not no-go; correct the embedder dimension limit [State: Done, Priority: High]
- [x] **DRAG-54**: test(eval): SWE-bench Verified importer and loom-eval swebench runner [State: Done, Priority: High]
- [ ] **DRAG-55**: test(eval): SWE-bench baselines, pooling and query variants (local models) [State: In Progress, Priority: High]
- [x] **DRAG-56**: feat(embed): embedding dimensions (MRL), model@dims cache key, retries and token budget [State: Done, Priority: Medium]
- [ ] **DRAG-58**: test(eval): pre-registered re-run of the LLM contextual prefix experiment [State: Backlog, Priority: Medium]
- [x] **DRAG-59**: docs: crate READMEs and a documentation audit of the whole project [State: Done, Priority: High]
- [ ] **DRAG-60**: feat(dendrite-rag): improvements for the Dendrite integration [State: To Verify, Priority: Medium]
- [x] **DRAG-61**: test(retrieve): measure or revert adaptive fusion weights, corpus auto routing and ALL_CAPS symbols [State: Done, Priority: High]
- [x] **DRAG-62**: fix: review findings in embeddings (dimensions, retries, budget), SWE-bench metrics and the RPC surface [State: Done, Priority: High]
- [x] **DRAG-63**: test: parameterize repetitive unit tests with rstest [State: Done, Priority: Low]

### RAG graph + Postgres

- [x] **DRAG-22**: fix(vector): crash-safe vectors.bin (fsync, u16 lengths, checksum) that self-heals from the embedding cache [State: Done, Priority: High]
- [x] **DRAG-23**: perf(search): LRU cache of query embeddings keyed by model and query [State: Done, Priority: High]
- [x] **DRAG-24**: docs: drop the removed Go fallback and hnsw_rs from ARCHITECTURE and retrieval specs [State: Done, Priority: Medium]
- [x] **DRAG-25**: test(eval): golden sets for embedag and dendrite-rag-rs plus broad questions [State: Done, Priority: High]
- [x] **DRAG-26**: feat(search): per-embedder fusion weights in rag.toml tuned by an eval sweep [State: Done, Priority: High]
- [x] **DRAG-27**: feat(rerank): in-process candle cross-encoder reranker (bge-reranker) selectable in rag.toml [State: Done, Priority: Medium]
- [x] **DRAG-28**: feat(graph): typed call/type/import edges, import resolution per language, ledger schema v2 [State: Done, Priority: High]
- [x] **DRAG-29**: feat(graph): pure-Rust Leiden communities stored per file and chunk in the ledger [State: Done, Priority: High]
- [x] **DRAG-30**: refactor(store): Store traits behind Engine with LocalStore (redb, Tantivy, FlatIndex) [State: Done, Priority: High]
- [x] **DRAG-31**: feat(search): community-aware context expansion and per-community summary chunks [State: Done, Priority: High]
- [x] **DRAG-32**: feat(rpc): graph.communities, graph.god_nodes, graph.path, graph.explain (protocol 1.1.0) and CLI [State: Done, Priority: High]
- [x] **DRAG-33**: feat(store): PgStore remote mode on Postgres + pgvector with SQL BM25 and shared embeddings [State: Done, Priority: High]
- [x] **DRAG-34**: feat(store): edge mode with WAL journal and replay, Merkle-root pull, pull --slice [State: Done, Priority: Medium]
- [x] **DRAG-35**: feat(store): push with Tier 2 compaction and --dry-run report [State: Done, Priority: Medium]
- [x] **DRAG-36**: feat(llm): optional LiteLLM concept extraction, community summaries and contextual headers [State: Done, Priority: Low]
- [x] **DRAG-37**: feat(vector): HNSW behind VectorIndex for large corpora (recall@10 >= 0.95) [State: Done, Priority: Low]

### Rust RAG only + quality gate

- [x] **DRAG-38**: feat(ask): retrieval-augmented generation in the sidecar (ask RPC: prompts, citations, local via gateway, hybrid and paid tiers) [State: Done, Priority: High]
- [x] **DRAG-39**: feat(rank): rank RPC for hybrid ranking of supplied documents (agent routing) [State: Done, Priority: High]
- [ ] **DRAG-40**: ci(quality): max 500 lines per code file and jscpd duplicate gate; split the 14 long Rust files [State: Backlog, Priority: High]
- [x] **DRAG-41**: perf(vector): benchmark flat vs HNSW from 50k to 400k vectors and set the auto threshold from the crossover [State: Done, Priority: Medium]
- [x] **DRAG-42**: perf(store): measure cold edge indexing at 50k+ chunks and decide on index segment pull [State: Done, Priority: Medium]
- [ ] **DRAG-43**: perf(vector): replace or rewrite HNSW so an approximate index beats the flat scan above 200k vectors [State: Backlog, Priority: Medium]
- [x] **DRAG-44**: perf(graph): code graph build peaks at 463 MB at 49k chunks; stream chunks without their text to stay under the 200 MB budget [State: Done, Priority: High]

