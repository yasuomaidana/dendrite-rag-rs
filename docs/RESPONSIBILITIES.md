# dendrite-rag-rs Responsibilities & Architecture Boundary

## 1. Purpose

`dendrite-rag-rs` is the dedicated Rust repository for Dendrite's hybrid retrieval-augmented generation (RAG) engine, BLAKE3 content-addressable segmented store (CAS), and distributed memory synchronization engine (Pillar 7 & Pillar 9). It provides deterministic sparse (Okapi BM25) and dense (vector embedding) indexing, Reciprocal Rank Fusion (RRF) ranking, structured topic memory parsing with zero-extrapolation fidelity guards, and multi-tier memory synchronization (`local-only`, `edge` WAL, and `push` compaction). Currently initialized as a minimal Rust library scaffold (`src/lib.rs`), it is the extraction target for Dendrite's retrieval, segmented storage, and memory subsystems under umbrella ticket `DEN-974` (`DRAG-1`).

---

## 2. Owns / Does NOT Own

### Owns (`dendrite-rag-rs`)
- **Hybrid BM25 + Vector Retrieval & Ranking**:
  - Incremental project indexing and AST/markdown/text chunking (extracting from `pkg/rag/indexer.go` and `pkg/rag/types.go`).
  - Sparse Okapi BM25 lexical scoring and dense vector similarity scoring with Reciprocal Rank Fusion (`k = 60.0`) (extracting from `pkg/rag/indexer.go` and `pkg/agentsearch/{bm25.go,embedding.go,rrf.go}`).
  - Local-first RAG cascade using zero-cost `nomic-embed-text` embeddings with a `0.72` cosine similarity confidence threshold before paid LLM fallback (`DEND-203`, Planned).
- **Content-Addressable Store (CAS) & Segmented Persistence**:
  - BLAKE3 content-addressed persistent segmented store (`meta.db` SQLite metadata + Zstd-compressed blobs in `~/.dendrite/store/<project-hash>/`) and chunk state checkpointer (`STORED`, `ANALYZING_PARTIAL`, `QUEUED_NIGHT_LOCAL`, `PAUSED_QUOTA_FLOOR`) (`DEN-059`, extracting from `pkg/store/segmented`).
  - Shared library deduplication across projects via global CAS (`~/.dendrite/cache/cas/<blake3_root>.idx`, `DEND-203`, Planned).
  - Store inspection, chunk status telemetry, and explicit purge lifecycle (`DEN-060`, extracting from `pkg/store/status`).
- **Distributed Memory Synchronization Engine (Pillar 7)**:
  - Three explicit synchronization modes bridging Tier 1 local storage (`~/.dendrite/store` SQLite + blob CAS) and Tier 2 shared storage (PostgreSQL 16 + `pgvector` on `dendrite-server`) (`DEN-964`, Planned):
    1. `local-only`: Strict offline execution against the local `dendrite pool` DAG slice with zero network calls.
    2. `edge`: Read-through cache from central PostgreSQL + `pgvector` with local write-ahead logging (`.dendrite/memory-wal.journal`) and 30-second reconnect background replay.
    3. `push`: Delta upload of local chunks/embeddings/session summaries plus server-side compaction (pruning unreachable commit slices, deleting orphaned CAS blobs, and merging duplicate vector chunks with cosine similarity `> 0.995`).
- **Structured Topic Memory & Zero-Extrapolation Guard**:
  - Emoji-headed structured topic memory parsing/formatting and `FidelityGuard` verification (enforcing verbatim mandatory disclaimers, key entity retention, and blocking extrapolated terms) (`DEN-803`, extracting from `pkg/personalcontext/memory/memory.go`).

### Does NOT Own (and Owning Repository)
- **`dendrite-control` (`Dendrite` Go control-plane repo, module `dendrite.dev/control`)**:
  - CLI command wiring and interactive terminal prompts (`cmd/dendrite/ask.go`, `cmd/dendrite/store.go`, `cmd/dendrite/agent.go`, `cmd/dendrite/query.go`).
  - Control-plane HTTP server routing (`pkg/server/server.go` hosting `/api/v1/rag/ask` and `/api/v1/rag/retrieve`), Web UI store handlers (`pkg/ui/store`), LiteLLM token budget governance (`pkg/gateway`), Plane v2 tracker synchronization (`pkg/tracker`), multi-repo workspace orchestration (`pkg/pool`), and agent runner / session ledger scoring (`pkg/agent`, `pkg/agentsearch/scorer.go`).
- **`dendrite-mcp-hub` (Go module `dendrite.dev/mcphub`)**:
  - MCP server protocol transport, tool registration (`pkg/mcpserver/tools_rag.go` exposing `dendrite_rag_retrieve` and `dendrite_rag_ask`), JetBrains IDE bridge, and Penpot/Plane/Engram MCP upstreams (`DEN-971`).
- **`dendrite-core-rs` (Rust crate `dendrite-core`)**:
  - Tree-sitter AST folding, last-node extraction, cyclomatic and cognitive complexity calculation, core BLAKE3 file/AST hashing primitives, token counting, and CFG slicing (`DEN-972`).
- **`dendrite-git-rs` (Rust crate `dendrite-git`)**:
  - `gix` hunk dependency graph construction, semantic hunk clustering (`HunkClusterer`), and stacked review-branch splitting (`DEN-973`).
- **`embedag` (`../../RustRoverProjects/embedag/crates/embedag`)**:
  - Foundational Rust DAG, graph-indexing, and matcher primitives consumed as an upstream Rust crate library.

---

## 3. Interfaces

### What `dendrite-rag-rs` Exposes
- **Sidecar Binary (`dendrite-rag`, Planned under `DEN-970` / `DEN-974`)**:
  - Speaks **JSON-RPC 2.0 over stdio** with request/response schemas defined in `proto/*.json` and versioned via SemVer.
  - Exposes health handshake, incremental project indexing, hybrid BM25 + vector chunk retrieval, CAS segment commit/query/inspect/purge operations, memory sync mode execution (`local-only`, `edge`, `push`), and topic memory fidelity verification.
- **Rust Crate API (`dendrite-rag` / `dendrite_rag_rs`)**:
  - Library crate (`src/lib.rs`, currently containing bootstrap stub `pub fn init() -> bool`) exposing retrieval, CAS, and memory sync primitives for direct Rust integration and unit/property testing.

### Who Consumes It
- **`dendrite-control` (`Dendrite` Go repo)**:
  - Invokes the `dendrite-rag` binary via `pkg/sidecar.Client` (`DEN-970`) with a startup health handshake and automatic fallback to the in-process Go implementations (`pkg/rag`, `pkg/store/segmented`, `pkg/store/status`, `pkg/agentsearch`, `pkg/personalcontext/memory`) when the sidecar binary is absent or unhealthy.
  - Consumed by `dendrite ask` (`cmd/dendrite/ask.go`), `dendrite store` (`cmd/dendrite/store.go`), `dendrite query` (`cmd/dendrite/query.go`), `dendrite agent search/route` (`cmd/dendrite/agent.go`), and HTTP endpoints `POST /api/v1/rag/ask` and `POST /api/v1/rag/retrieve` (`pkg/server/server.go`).
- **`dendrite-mcp-hub`**:
  - Exposes MCP tools `dendrite_rag_retrieve` and `dendrite_rag_ask` (`pkg/mcpserver/tools_rag.go`) to IDE and autonomous coding agents, delegating retrieval and indexing to the RAG engine.

---

## 4. Code Being Extracted / Replaced & Go Fallback Behaviour

### Existing Dendrite Go Packages & Verified Files
1. **RAG Indexing & Retrieval (`pkg/rag/`)**:
   - `pkg/rag/types.go`: `Chunk`, `SearchOptions`, `AskRequest`, `AskResult`, `PaidCallerFunc`.
   - `pkg/rag/indexer.go`: `Indexer`, `FileMeta`, `IndexCache` (persisted at `.dendrite/rag_cache.json`), `Tokenize`, `AddChunk`, `RemoveFile`, `IndexProject`, `IndexProjectIncremental`, `chunkGoAST`, `chunkMarkdown`, `chunkCodeOrText`, and Okapi BM25 `Retrieve` (`k1 = 1.5`, `b = 0.75`).
   - `pkg/rag/ask.go`: `AskEngine` (`Ask`, `delegateToRemoteServer`, `tryLocalLLM`, `callPaidLLM`, `buildHybridRefinementPrompt`, `enhanceAnswer`).
   - `pkg/rag/indexer_test.go` and `pkg/rag/ask_test.go`.
2. **Hybrid BM25 + Embedding Search & RRF (`pkg/agentsearch/`)**:
   - `pkg/agentsearch/types.go`: `AgentID`, `CapabilityScore`, `TaskVector`, `LeidenCluster`, `AgentManifest`, `SearchQuery`, `SearchResult`, `RoutingDecision`.
   - `pkg/agentsearch/bm25.go`: `BM25Index` (`NewBM25Index`, `AddDocument`, `Score`, `Tokenize`).
   - `pkg/agentsearch/embedding.go`: `EmbeddingClient` interface, `TEIEmbeddingClient` (`POST /embed`, default dim 384), `OllamaEmbeddingClient` (`POST /api/embeddings` with `nomic-embed-text`, default dim 768), and `CosineSimilarity`.
   - `pkg/agentsearch/rrf.go`: `ReciprocalRankFusion` (`k = 60.0` merging dense and sparse rankings).
   - `pkg/agentsearch/engine.go`, `pkg/agentsearch/scorer.go`, `pkg/agentsearch/engine_test.go` (note: agent manifest scoring against `agent.SessionLedger` and runner dispatch remain in `dendrite-control`).
3. **BLAKE3 Segmented Store & Inspection (`pkg/store/segmented/`, `pkg/store/status/`)**:
   - `pkg/store/segmented/types.go`: `ChunkState` (`STORED`, `ANALYZING_PARTIAL`, `QUEUED_NIGHT_LOCAL`, `PAUSED_QUOTA_FLOOR`), `SegmentType` (`file`, `function`, `cfg_block`, `ast_graph`), `Segment`, `ChunkCheckpoint`, plus `SegmentedStore` methods (`NewSegmentedStore`, `CommitChunk`, `ListCheckpoints`, `QueryPartial`, `PurgeProject`, `ComputeHash`) invoked by `cmd/dendrite/store.go`.
   - `pkg/store/status/types.go` & `pkg/store/status/inspector.go` (`pkg/store/status/inspector_test.go`): `StoreInspector` (`InspectStore`, `InspectStoreDetails`, `RemoveStore`, `FormatTable`) reading `meta.db` and `blobs/` under `~/.dendrite/store/<project-hash>/`.
4. **Structured Topic Memory & Zero-Extrapolation Guard (`pkg/personalcontext/memory/`)**:
   - `pkg/personalcontext/memory/memory.go`: `TopicMemory`, `TopicSection`, `Parser` (`Parse`, `Format`), `FidelityReport`, and `FidelityGuard` (`Verify` checking verbatim `MandatoryDisclaimer`, `ExtrapolatedTerms`, and `MissingKeyEntities`).
5. **Distributed Memory Sync (`pkg/memory/`, Planned)**:
   - Planned under `DEN-964` (`2026-10-quality-audit-execution-plan.md` §4.5) as `pkg/memory/{localonly,edge,push}` and targeted for Rust implementation in `dendrite-rag-rs`.

### Go Fallback Behaviour
Per `docs/architecture/2026-10-quality-audit-execution-plan.md` (§5), `dendrite-control` wraps sidecar calls in `sidecar.Client`. At startup or invocation, `sidecar.Client` performs a JSON-RPC 2.0 health handshake with the `dendrite-rag` binary over stdio. When the `dendrite-rag` binary is missing, incompatible in SemVer schema, or unhealthy, `dendrite-control` transparently falls back to the in-process Go implementations in `pkg/rag`, `pkg/store/segmented`, `pkg/store/status`, `pkg/agentsearch`, and `pkg/personalcontext/memory` without failing user commands.

---

## 5. Dependencies on Sibling Repositories

- **`embedag` (`../../RustRoverProjects/embedag/crates/embedag`)** *(Planned)*:
  - Consumed via Cargo path dependency with `default-features = false, features = ["core", "matcher"]` (shared convention with `dendrite-core-rs`; currently `Cargo.toml` in `dendrite-rag-rs` has an empty `[dependencies]` table awaiting implementation).
- **`dendrite-core-rs` (`../dendrite-core-rs`, crate `dendrite-core`)** *(Planned)*:
  - Synchronized across sibling checkouts via `dendrite pool resolve`, which writes a `[patch."https://github.com/yasuomaidana/dendrite-core-rs"] dendrite-core = { path = "../dendrite-core-rs" }` override in consuming crates. Provides canonical BLAKE3 hashing, AST symbol extraction, and token counting so chunk hashes (`blake3(path + symbol + content)`) match across `dendrite-core-rs` and `dendrite-rag-rs`.
- **`dendrite-control` (`../dendrite-control` / `../Dendrite`)**:
  - Consumes `dendrite-rag-rs` as a JSON-RPC 2.0 stdio sidecar and orchestrates multi-directory workspace resolution (`dendrite pool`).

---

## 6. Ticket Map

Tracking lives in Plane v2 project **DRAG** (workspace `dendrite-ws`, `http://localhost:8090`), under extraction umbrella **DEN-974** and first child **DRAG-1**.

| Capability Area | Ticket | Scope & Disposition | Status |
| :--- | :--- | :--- | :--- |
| **Sidecar Extraction & Contract** | **DEN-974 / DRAG-1** | Extract `dendrite-rag-rs` crate & `dendrite-rag` JSON-RPC 2.0 stdio binary (`proto/*.json` schema, Go `sidecar.Client` fallback) | Planned / In Progress |
| **Hybrid Retrieval & Local-First RAG** | **DEND-203** | Local-First RAG (`nomic-embed-text` >= 0.72 threshold) & Shared Library CAS Deduplication (`~/.dendrite/cache/cas/`) (move) | In Progress |
| **Hybrid Agent Search & Routing** | **DEN-037** | Hybrid Agent Search, TEI/Ollama Embeddings, BM25 & RRF Semantic Routing (`pkg/agentsearch`, `cmd/dendrite`) (split with `dendrite-control`) | In Progress |
| **BLAKE3 Segmented Store (CAS)** | **DEN-059** | BLAKE3 Content-Addressed Persistent Segmented Store & Chunk Checkpointer (`pkg/store/segmented`, `cmd/dendrite`) (split with `dendrite-control`) | To Verify |
| **Store Telemetry & Purge Lifecycle** | **DEN-060** | Store Inspection, Chunk Status Telemetry & Explicit User Purge Lifecycler (`pkg/store/status`, `pkg/ui/store`) (split with `dendrite-control`) | To Verify |
| **Structured Topic Memory Guard** | **DEN-803** | Structured Topic Memory Parser & Zero-Extrapolation Fidelity Guard (`pkg/personalcontext/memory`) (move) | To Verify |
| **Distributed Memory Sync Modes** | **DEN-964** | Memory modes: `local-only` / `edge` (WAL) / `push` (+compaction) (`pkg/memory` / `dendrite-rag-rs`) (move) | Backlog |

### Plane keys (synced with Plane on 2026-10-18)

Each relocated ticket has its own key in `DRAG`. The DEN original either is archived with the `Relocated` state (**move**) or stays open in DEN as the Go-side umbrella with the `relocated-split` label (**split**).

| DRAG key | Origin in DEN | Action | DRAG state |
| :--- | :--- | :--- | :--- |
| **DRAG-1** | umbrella DEN-974 | first child | Backlog |
| **DRAG-2** | DEN-964 (DEN #189) | move | Backlog |
| **DRAG-3** | DEN-803 (DEN #131) | move | To Verify |
| **DRAG-4** | DEND-203 (DEN #108) | move | In Progress |
| **DRAG-5** | DEN-060 (DEN #60) | split | To Verify |
| **DRAG-6** | DEN-059 (DEN #59) | split | To Verify |
| **DRAG-7** | DEN-037 (DEN #37) | split | In Progress |

---

## 7. Imported Docs

The following documentation files from `Dendrite` are listed in [`docs/imports.json`](imports.json) for import under `docs/imported/`:

- `docs/architecture/2026-10-quality-audit-execution-plan.md`: Defines Pillar 7 distributed memory sync modes (`local-only`, `edge`, `push` in §4.5) and Pillar 9 five-subproject decomposition, sidecar JSON-RPC 2.0 contract, and extraction order (§5).
- `docs/mcp/dendrite_rag_retrieve.md`: Specifies the `dendrite_rag_retrieve` MCP tool input/output schema and top-K BM25 chunk retrieval behavior without LLM invocation.
- `docs/mcp/dendrite_rag_ask.md`: Specifies the `dendrite_rag_ask` MCP tool input/output schema, execution modes (`local`, `hybrid`, `paid`, `central-rag`), and source citation format.
- `docs/guides/asking-questions-to-dendrite.md`: Documents the end-to-end RAG indexing pipeline, `.dendrite/rag_cache.json` incremental cache, AST-bounded Go chunking, and central RAG HTTP endpoints (`/api/v1/rag/ask`, `/api/v1/rag/retrieve`).
- `docs/features/FEAT-022-agent-search-and-capability-discovery.md`: Specifies the hybrid dense vector (TEI/Ollama) + sparse BM25 + Reciprocal Rank Fusion (`k = 60.0`) pipeline implemented in `pkg/agentsearch`.
- `docs/features/FEAT-031-persistent-store-hybrid-token-scheduler.md`: Specifies the BLAKE3 content-addressed persistent segmented store (`dendrite store`), `meta.db` schema, Zstd blob layout, and chunk checkpoint states (`DEN-059`, `DEN-060`).
- `docs/features/FEAT-045-code-knowledge-mcp-graphify.md`: Specifies the local-first RAG cascade (`nomic-embed-text` with `0.72` similarity threshold) and global CAS shared library deduplication (`DEND-203`).
- `docs/cli/ask.md`: Documents the `dendrite ask` CLI flags (`--mode`, `--retrieve-only`, `--top-k`, `--remote-url`, `--paid-model`) that drive the RAG engine.

---

## 8. Open Questions & Boundary Conflicts

1. **Crate and Binary Naming Alignment (`dendrite-rag` vs `dendrite-rag-rs`)**:
   - `2026-10-quality-audit-execution-plan.md` §5 specifies the Rust crate name as `dendrite-rag` and the sidecar binary as `dendrite-rag`, whereas `Cargo.toml` currently sets `name = "dendrite-rag-rs"` with no `[[bin]]` entry or `proto/*.json` directory yet.
2. **BLAKE3 & AST Chunking Boundary Between `dendrite-core-rs` and `dendrite-rag-rs`**:
   - `pkg/rag/indexer.go` currently performs its own Go AST chunking (`chunkGoAST`) and cyclomatic/cognitive complexity annotations, while `dendrite-core-rs` (`DEN-972`) owns tree-sitter folding, complexity metrics, and BLAKE3 hashing. The exact boundary—whether `dendrite-rag-rs` links `dendrite-core` directly for AST chunking/BLAKE3 digests or receives pre-sliced chunks over RPC—must be locked in `proto/*.json`.
3. **Retrieval vs. LLM Synthesis Split in `pkg/rag/ask.go`**:
   - `pkg/rag/ask.go` currently couples BM25 retrieval (`Indexer.Retrieve`) with local Ollama / paid LiteLLM chat completion calls and interactive terminal confirmation. During extraction, pure indexing and hybrid retrieval belong in `dendrite-rag-rs`, while LLM provider routing, token governance, and interactive CLI prompts remain in `dendrite-control`.
4. **Split Ownership in `DEN-037` (`pkg/agentsearch`)**:
   - `pkg/agentsearch` mixes general-purpose hybrid search primitives (`BM25Index`, `TEIEmbeddingClient`, `OllamaEmbeddingClient`, `ReciprocalRankFusion`) with control-plane agent manifest routing (`Scorer` using `agent.SessionLedger` and `cmd/dendrite/agent.go`). Only the hybrid BM25 + vector + RRF retrieval engine belongs in `dendrite-rag-rs`.
5. **Split Ownership in `DEN-059` & `DEN-060` (`pkg/store/*` vs CLI/UI)**:
   - Storage persistence (`pkg/store/segmented`) and store inspection/purge logic (`pkg/store/status`) belong in `dendrite-rag-rs`, while CLI subcommands (`cmd/dendrite/store.go`) and web dashboard views (`pkg/ui/store`) remain in `dendrite-control`.
6. **Direct vs. Proxied Sidecar Invocation from `dendrite-mcp-hub`**:
   - `pkg/mcpserver/tools_rag.go` (moving to `dendrite-mcp-hub`) currently calls `pkg/rag` in-process. Once extracted, `dendrite-mcp-hub` and `dendrite-control` must agree on whether `dendrite-mcp-hub` spawns its own `dendrite-rag` stdio sidecar or delegates through `dendrite-control`.

## 9. Planned Engine Stack (decided 2026-10-18, pending benchmark gate)

Selection criteria: pure Rust (no C FFI), < 200 MB baseline RSS, concurrent readers, incremental updates.

| Concern | Choice | Rejected (reason) |
|---|---|---|
| Lexical / BM25 | **Tantivy** (mmap segments, lock-free `Searcher` pool) | SQLite FTS5 (bundled C FFI) |
| Vector ANN | **hnsw_rs** (cosine, M=16, ef_construction=200; dump/reload; incremental insert) | instant-distance (rebuild-only), sqlite-vec (C extension), embedded LanceDB (footprint, heavy build) |
| Embeddings | **candle** in-process embedder reused from `embedag-llm` (384-d bge-small/MiniLM class) | fastembed-rs (`ort` → ONNX Runtime C++); OxiLLaMa `/v1/embeddings` only as fallback with a dedicated embedding GGUF |
| Fusion | **RRF** reused from `embedag-rag::rrf` (instead of re-porting `pkg/agentsearch/rrf.go`) | — |
| Chunk ledger | BLAKE3 content-addressed ledger in **redb** (pure Rust) | SQLite `meta.db` of the current Go store (FFI) |

Benchmark gate before committing (criterion harness in `benches/`, Dendrite repo as corpus, candidates behind cargo features): RSS < 200 MB at 50k chunks, p95 query latency < 30 ms with 8 concurrent readers, recall@10 ≥ 0.95 versus brute force.
Source: Dendrite plan "Architectural Planning & Execution Blueprint", §3 (RAG Architecture & embedag Specification).
