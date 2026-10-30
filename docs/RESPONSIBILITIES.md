# dendrite-rag-rs Responsibilities & Architecture Boundary

## 1. Purpose

`dendrite-rag-rs` is the dedicated Rust repository for Dendrite's hybrid retrieval-augmented generation (RAG) engine, BLAKE3 content-addressable segmented store (CAS, planned), and distributed memory synchronization engine (Pillar 7 & Pillar 9). It provides deterministic sparse (Okapi BM25) and dense (vector embedding) indexing, Reciprocal Rank Fusion (RRF) ranking, structured topic memory parsing with zero-extrapolation fidelity guards (planned), and multi-tier memory synchronization (`edge` WAL and `push` compaction implemented; `local-only` slice mode planned). It is a Cargo workspace: `loom-rag` (the engine library `loom_rag`, free of Dendrite references), `dendrite-rag` (the sidecar binary and library `dendrite_rag`: JSON-RPC protocol 1.9.0, `.dendrite/rag.toml` settings, Dendrite's `ask`) and `loom-eval` (evaluation). It is the extraction target for Dendrite's retrieval, segmented storage, and memory subsystems under umbrella ticket `DEN-974` (`DRAG-1`).

---

## 2. Owns / Does NOT Own

### Owns (`dendrite-rag-rs`)
- **Hybrid BM25 + Vector Retrieval & Ranking**:
  - Incremental project indexing and AST/markdown/text chunking (extracting from `pkg/rag/indexer.go` and `pkg/rag/types.go`).
  - Sparse Okapi BM25 lexical scoring (`[lexical] k1, b, q`, DRAG-47) and dense vector similarity scoring (flat int8 scan, HNSW behind feature `hnsw` and `[index] vector_index`: DRAG-37/41) with Reciprocal Rank Fusion (`k = 60.0`, extracting from `pkg/rag/indexer.go` and `pkg/agentsearch/{bm25.go,embedding.go,rrf.go}`).
  - Local-first RAG cascade with prompt/answer caching, local model drafting through the LiteLLM gateway, and paid refinement / escalation (`DEND-203` / `ask`).
- **Content-Addressable Store (CAS) & Segmented Persistence** *(Planned: DRAG-5 / DRAG-6)*:
  - BLAKE3 content-addressed persistent segmented store (`meta.db` SQLite metadata + Zstd-compressed blobs in `~/.dendrite/store/<project-hash>/`) and chunk state checkpointer (`STORED`, `ANALYZING_PARTIAL`, `QUEUED_NIGHT_LOCAL`, `PAUSED_QUOTA_FLOOR`) (`DEN-059`, extracting from `pkg/store/segmented`).
  - Shared library deduplication across projects via global CAS (`~/.dendrite/cache/cas/<blake3_root>.idx`, `DEND-203`, Planned).
  - Store inspection, chunk status telemetry, and explicit purge lifecycle (`DEN-060`, extracting from `pkg/store/status`).
- **Distributed Memory Synchronization Engine (Pillar 7)**:
  - Synchronization modes bridging Tier 1 local storage (redb default, SQLite opt-in: `[storage] ledger = "sqlite"`, feature `sqlite`, DRAG-49) and Tier 2 shared storage (PostgreSQL 16 + `pgvector` on `dendrite-server`, DRAG-33):
    1. `local-only` *(Planned: DRAG-2)*: Strict offline execution against the local `dendrite pool` DAG slice with zero network calls.
    2. `edge` *(Implemented: DRAG-34)*: Read-through cache from central PostgreSQL + `pgvector` with local write-ahead logging (`.dendrite/memory-wal.journal`) and 30-second reconnect background replay.
    3. `push` *(Implemented: DRAG-35)*: Delta upload of local chunks, embeddings, and metadata plus server-side compaction (pruning unreachable commit slices, deleting orphaned embeddings and postings, and merging duplicate vector chunks with cosine similarity `> 0.995`).
- **Structured Topic Memory & Zero-Extrapolation Guard** *(Planned: DRAG-3)*:
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
- **Sidecar Binary (`dendrite-rag`, Implemented: DRAG-1)**:
  - Speaks **JSON-RPC 2.0 over stdio** (protocol 1.9.0) with request/response schemas defined in `proto/*.json` and versioned via SemVer.
  - Exposes health handshake, incremental project indexing, hybrid BM25 + vector chunk retrieval, token-budgeted context packs, symbol navigation, graph queries, history search, document ranking, memory sync (`sync`, `push`, `pull`), and `ask`. (Segmented CAS operations, topic memory verification, local-only slice sync, and `index.drain` do not exist).
- **Rust Crate API**:
  - `loom_rag`: the retrieval engine (indexing, corpora, search, context packs, stores) for direct Rust integration; no Dendrite names, settings files or environment variables.
  - `dendrite_rag`: the sidecar's settings resolution, server and `ask`, on top of `loom_rag`.

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
5. **Distributed Memory Sync (`pkg/memory/`)**:
   - `edge` mode with write journal (DRAG-34) and `push` with compaction (DRAG-35) are implemented in `loom-rag` / `dendrite-rag`; `local-only` slice mode (DRAG-2) remains planned under `DEN-964`.

### Go Fallback Behaviour
Per `docs/architecture/2026-10-quality-audit-execution-plan.md` (§5), `dendrite-control` wraps sidecar calls in `sidecar.Client`. At startup or invocation, `sidecar.Client` performs a JSON-RPC 2.0 health handshake with the `dendrite-rag` binary over stdio. When the `dendrite-rag` binary is missing, incompatible in SemVer schema, or unhealthy, `dendrite-control` transparently falls back to the in-process Go implementations in `pkg/rag`, `pkg/store/segmented`, `pkg/store/status`, `pkg/agentsearch`, and `pkg/personalcontext/memory` without failing user commands.

---

## 5. Workspace Dependencies & Sibling Repositories

### Sibling Repositories
- **`embedag`**: Consumed by `loom-rag` via git dependency (`git = "https://github.com/yasuomaidana/embedag", branch = "develop", version = "0.1.0", default-features = false, features = ["core"]`) for BLAKE3 Merkle tree change detection (`MerkleNode::from_paths` and `diff`).
- **`dendrite-core-rs` (`../dendrite-core-rs`, crate `dendrite-core`)** *(Planned)*:
  - Planned future parser integration. Currently `loom-rag` links tree-sitter grammars directly and hashes chunks with `blake3`; `dendrite-core-rs` keeps signature folding, complexity metrics and token counting.
- **`dendrite-control` (`../dendrite-control` / `../Dendrite`)**:
  - Consumes `dendrite-rag` as a JSON-RPC 2.0 stdio sidecar (protocol 1.9.0) and orchestrates multi-directory workspace resolution (`dendrite pool`).

### Crate Dependencies (`Cargo.toml`)

#### `loom-rag` (`crates/loom-rag/Cargo.toml`)

| Dependency | Version / Source | Purpose / Flags |
|---|---|---|
| `anyhow` | `1` | Error handling |
| `blake3` | `1.8.7` | Content hashing, Merkle nodes, vector checksums |
| `embedag` | `git = "https://github.com/yasuomaidana/embedag", branch = "develop", version = "0.1.0"` | `default-features = false, features = ["core"]` (Merkle tree change detection) |
| `globset` | `0.4.20` | Path glob matching filters |
| `ignore` | `0.4.33` | Gitignore-aware parallel filesystem walk |
| `lru` | `0.19.0` | Query vector caching |
| `rayon` | `1.12.0` | Parallel hashing, walking, and vector scanning |
| `redb` | `4.4.0` | Pure-Rust embedded key-value ledger store (default) |
| `regex` | `1.13.1` | Regular expressions |
| `serde` | `1.0.229` | `features = ["derive"]` serialization |
| `serde_json` | `1.0.154` | JSON serialization |
| `streaming-iterator` | `0.1.9` | Streaming iterator utilities |
| `tantivy` | `0.27.0` | BM25 lexical indexing and search |
| `thiserror` | `2.0.21` | Structured error derive |
| `tree-sitter` | `0.27.1` | AST parsing for code chunking |
| `tree-sitter-c` | `0.24.2` | C grammar |
| `tree-sitter-cpp` | `0.23.4` | C++ grammar |
| `tree-sitter-go` | `0.25.0` | Go grammar |
| `tree-sitter-java` | `0.23.5` | Java grammar |
| `tree-sitter-javascript` | `0.25.0` | JavaScript grammar |
| `tree-sitter-python` | `0.25.0` | Python grammar |
| `tree-sitter-rust` | `0.24.2` | Rust grammar |
| `tree-sitter-typescript` | `0.23.2` | TypeScript grammar |
| `ureq` | `3.5.0` | `features = ["json"]` HTTP client for embedders / rerankers |
| `candle-core` | `0.11` | Optional (feature `candle`) in-process tensor engine |
| `candle-nn` | `0.11` | Optional (feature `candle`) neural network layers |
| `candle-transformers` | `0.11` | Optional (feature `candle`) transformer architectures |
| `hf-hub` | `1.0.0` | Optional (features `candle`, `oxillama`) Hugging Face Hub client |
| `tokenizers` | `1.0.0-rc.3` | Optional (feature `candle`) fast tokenizers |
| `oxillama-runtime` | `0.1.4` | Optional (feature `oxillama`) in-process GGUF runtime |
| `pgvector` | `0.4.2` | Optional (feature `postgres`) PostgreSQL vector support |
| `postgres` | `0.19.14` | Optional (feature `postgres`) PostgreSQL client for shared tier |
| `model2vec-rs` | `0.3.0` | Optional (feature `model2vec`) static token embeddings |
| `rusqlite` | `=0.40.2` | Optional (feature `sqlite`, `features = ["bundled"]`) SQLite ledger |
| `tempfile` | `3.27.0` | Optional (feature `testutil`) temporary directory fixtures |
| `dhat` | `0.3.3` | Optional (feature `dhat`) heap profiling |

*Dev-dependencies:* `criterion = { version = "0.8.2", default-features = false, features = ["cargo_bench_support"] }`, `libc = "0.2.191"`, `tempfile = "3.27.0"`, `testcontainers = { version = "0.28.0", features = ["blocking"] }`.

#### `dendrite-rag` (`crates/dendrite-rag/Cargo.toml`)

| Dependency | Version / Source | Purpose / Flags |
|---|---|---|
| `anyhow` | `1` | Error handling |
| `blake3` | `1.8.7` | Content hashing |
| `clap` | `4.6.7` | `features = ["derive"]` CLI argument parsing |
| `loom-rag` | `path = "../loom-rag"` | Engine library |
| `serde` | `1.0.229` | `features = ["derive"]` serialization |
| `serde_json` | `1.0.154` | JSON-RPC 2.0 framing and serialization |
| `thiserror` | `2.0.21` | Structured error derive |
| `toml` | `1.1.8` | `default-features = false, features = ["parse", "display", "serde"]` for `.dendrite/rag.toml` |

*Dev-dependencies:* `loom-rag = { path = "../loom-rag", features = ["testutil"] }`, `libc = "0.2.191"`, `tempfile = "3.27.0"`.

#### `loom-eval` (`crates/loom-eval/Cargo.toml`)

| Dependency | Version / Source | Purpose / Flags |
|---|---|---|
| `anyhow` | `1` | Error handling |
| `clap` | `4.6.7` | `features = ["derive"]` CLI argument parsing |
| `dendrite-rag` | `path = "../dendrite-rag"` | Sidecar adapter library |
| `loom-rag` | `path = "../loom-rag"` | Engine library |
| `serde` | `1.0.229` | `features = ["derive"]` serialization |
| `serde_json` | `1.0.154` | Benchmark / golden dataset JSON handling |
| `tempfile` | `3.27.0` | Isolated benchmark directories |

---

## 6. Ticket Map

Tracking lives in Plane v2 project **DRAG** (workspace `dendrite-ws`, `http://localhost:8090`), under extraction umbrella **DEN-974** and first child **DRAG-1**.

| Capability Area | Ticket | Scope & Disposition | Status |
| :--- | :--- | :--- | :--- |
| **Sidecar Extraction & Contract** | **DEN-974 / DRAG-1** | Extract `dendrite-rag-rs` workspace & `dendrite-rag` JSON-RPC 2.0 stdio binary (protocol 1.9.0, `proto/*.json` schema) | Implemented |
| **Hybrid Retrieval & Local-First RAG** | **DEND-203 / DRAG-4** | Local-first RAG & hybrid retrieval (`ask`, BM25, vector search, RRF fusion, context packing) implemented; Shared library CAS deduplication (`~/.dendrite/cache/cas/`) planned | Implemented / Planned |
| **Hybrid Agent Search & Document Ranking** | **DEN-037 / DRAG-7** | Hybrid search & ranking (`rank`) implemented; control-plane agent manifest routing in `dendrite-control` | Implemented / Split |
| **BLAKE3 Segmented Store (CAS)** | **DEN-059 / DRAG-6** | BLAKE3 Content-Addressed Persistent Segmented Store & Chunk Checkpointer (`pkg/store/segmented`, `cmd/dendrite`) | Planned (To Verify) |
| **Store Telemetry & Purge Lifecycle** | **DEN-060 / DRAG-5** | Store Inspection, Chunk Status Telemetry & Explicit User Purge Lifecycler (`pkg/store/status`, `pkg/ui/store`) | Planned (To Verify) |
| **Structured Topic Memory Guard** | **DEN-803 / DRAG-3** | Structured Topic Memory Parser & Zero-Extrapolation Fidelity Guard (`pkg/personalcontext/memory`) | Planned (To Verify) |
| **Distributed Memory Sync Modes** | **DEN-964 / DRAG-2** | Memory modes: `edge` WAL (DRAG-34) and `push` (+compaction, DRAG-35) implemented; `local-only` slice mode planned (DRAG-2) | Implemented / Planned |

### Plane keys (synced with Plane on 2026-10-18)

Each relocated ticket has its own key in `DRAG`. The DEN original either is archived with the `Relocated` state (**move**) or stays open in DEN as the Go-side umbrella with the `relocated-split` label (**split**).

| DRAG key | Origin in DEN | Action | DRAG state |
| :--- | :--- | :--- | :--- |
| **DRAG-1** | umbrella DEN-974 | first child | Implemented |
| **DRAG-2** | DEN-964 (DEN #189) | move | Planned (Backlog; edge DRAG-34 & push DRAG-35 implemented) |
| **DRAG-3** | DEN-803 (DEN #131) | move | Planned (To Verify) |
| **DRAG-4** | DEND-203 (DEN #108) | move | Implemented (CAS dedup planned) |
| **DRAG-5** | DEN-060 (DEN #60) | split | Planned (To Verify) |
| **DRAG-6** | DEN-059 (DEN #59) | split | Planned (To Verify) |
| **DRAG-7** | DEN-037 (DEN #37) | split | Implemented (split with `dendrite-control`) |

---

## 7. Specifications

The technical specification lives in this repository: [ARCHITECTURE.md](ARCHITECTURE.md) and [`specs/`](specs/). Dendrite's CLI and MCP reference pages (`dendrite ask`, `dendrite store`, `dendrite_rag_*`, `dendrite_store_*`) describe the commands Dendrite exposes on top of this engine and link here for the internals.

---

## 8. Open Questions & Boundary Conflicts

1. **Crate and Binary Naming** — *resolved (DRAG-9)*: package `dendrite-rag`, library `dendrite_rag`, binary `dendrite-rag`; the repository keeps the `-rs` suffix.
2. **BLAKE3 & AST Chunking Boundary Between `dendrite-core-rs` and `dendrite-rag-rs`**:
   - *Resolved (DRAG-10)*: retrieval chunking (cAST over tree-sitter) lives in dendrite-rag-rs, which links the tree-sitter grammars itself and hashes chunks with `blake3`. `dendrite-core-rs` keeps signature folding, complexity metrics and token counting; dendrite-rag-rs can move to a shared parser once core-rs publishes one.
3. **Retrieval vs. LLM Synthesis Split in `pkg/rag/ask.go`** — *resolved (2026-10-22)*:
   - Retrieval, prompts, the local / hybrid / paid tiers and the answer cache run in the sidecar's `ask` method (`crates/dendrite-rag/src/ask`), which calls models through the LiteLLM gateway (whose budgets `pkg/gateway` governs). `pkg/rag/ask.go` starts the sidecar, passes the gateway key, and asks the person before paid escalation (`escalation_needed`); `dendrite ask` searches code unless `-d` adds the documentation. The engine library `loom-rag` knows nothing of prompts or models beyond the optional graph LLM layer.
4. **Split Ownership in `DEN-037` (`pkg/agentsearch`)**:
   - `pkg/agentsearch` mixes general-purpose hybrid search primitives (`BM25Index`, `TEIEmbeddingClient`, `OllamaEmbeddingClient`, `ReciprocalRankFusion`) with control-plane agent manifest routing (`Scorer` using `agent.SessionLedger` and `cmd/dendrite/agent.go`). Only the hybrid BM25 + vector + RRF retrieval engine belongs in `dendrite-rag-rs`.
5. **Split Ownership in `DEN-059` & `DEN-060` (`pkg/store/*` vs CLI/UI)**:
   - Storage persistence (`pkg/store/segmented`) and store inspection/purge logic (`pkg/store/status`) belong in `dendrite-rag-rs`, while CLI subcommands (`cmd/dendrite/store.go`) and web dashboard views (`pkg/ui/store`) remain in `dendrite-control`.
6. **Direct vs. Proxied Sidecar Invocation from `dendrite-mcp-hub`**:
   - `pkg/mcpserver/tools_rag.go` (moving to `dendrite-mcp-hub`) currently calls `pkg/rag` in-process. Once extracted, `dendrite-mcp-hub` and `dendrite-control` must agree on whether `dendrite-mcp-hub` spawns its own `dendrite-rag` stdio sidecar or delegates through `dendrite-control`.

## 9. Engine Stack (decided 2026-10-18, revised 2026-10-20)

Selection criteria: no system libraries (bundled C compiled by `cc` is accepted, as for the tree-sitter grammars and, opt-in, SQLite), < 200 MB peak memory at 50k chunks, concurrent readers, incremental updates.

| Concern | Choice | Rejected (reason) |
|---|---|---|
| Chunking | **cAST** split-then-merge over **tree-sitter** syntax trees | fixed line windows (cut functions), whole files (dilute relevance) |
| Lexical / BM25 | **Tantivy** with a code tokenizer (camelCase / snake_case / digit split + whole identifier); configurable `[lexical] k1, b, q` (defaults unchanged: 1.2, 0.75, 1.0; DRAG-47) | SQLite FTS5 (bundled C FFI) |
| Vector search | exact parallel **int8** scan (4× smaller than f32, recall@10 ≥ 0.95); **HNSW** implemented behind feature `hnsw` and `[index] vector_index` (auto currently picks flat int8: DRAG-41) | hnsw_rs now (extra f32 copy + graph breaks the 200 MB budget at 50k chunks), instant-distance (rebuild-only), sqlite-vec (C extension), embedded LanceDB (footprint, heavy build) |
| Embeddings | OpenAI-compatible `/v1/embeddings` through the **LiteLLM gateway** (default), in-process **candle** (safetensors BERT/Nomic/Jina on Metal/CUDA/CPU), **ollama**, **hf**, **oxillama**, and in-process **model2vec** (offline) | fastembed-rs (`ort` → ONNX Runtime C++). The earlier plan to reuse a candle embedder from `embedag-llm` was dropped: its `CandleProvider` is a simulated structured-output provider, not an embedder |
| Fusion | weighted **RRF** (k = 60) over lexical, dense and symbol-name lists, implemented locally (no adaptive fusion weights) | `embedag-rag::RrfRanker` (two lists only, `Uuid` keys, pulls in `oxirag`) |
| Change detection | **embedag-core** BLAKE3 Merkle tree (`MerkleNode::from_paths` + `diff`, via git dependency `embedag`) | full rescans |
| Ledger | **redb** (pure Rust, default); **SQLite** (`rusqlite` bundled, feature `sqlite`, `[storage] ledger = "sqlite"`, DRAG-49) as an opt-in; shared tier **PostgreSQL 16 + pgvector** (remote: DRAG-33; edge mode with `memory-wal.journal`: DRAG-34; push and compaction: DRAG-35) | Turso (async-only Rust API in a synchronous engine, pre-1.0 file format, mimalloc and Tantivy in its default features; its MVCC writers do not help a single-writer engine; revisit at 1.0 with a sync API or an ANN index); sqlite-vec for search (int8 `vec0` exact scan p95 43 ms at 49k × 256-d with 8 readers, against 0.2 ms for the in-memory scan) |

Benchmark gate (DRAG-15, DRAG-44): cold index peak memory footprint at 49k chunks is ~180-200 MB (DRAG-44: 512-file group commits and streaming graph builder); p95 query latency < 30 ms with 8 concurrent readers; quantized recall@10 ≥ 0.95 versus exact f32 search; hybrid recall@10 ≥ lexical-only on the golden query set.
