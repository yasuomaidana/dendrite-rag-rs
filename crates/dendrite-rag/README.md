# dendrite-rag

[![Crate](https://img.shields.io/badge/crate-dendrite--rag-blue.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](../../Cargo.toml)

`dendrite-rag` is the retrieval sidecar and command-line tool for the Dendrite AI developer platform. It sits on the [`loom-rag`](../loom-rag) engine library and provides a line-delimited **JSON-RPC 2.0** server over stdio, a **CLI**, project settings from `.dendrite/rag.toml` with `DENDRITE_RAG_*` environment overrides, and the `ask` question-answering workflow.

---

## When to Use It

### When to use
- As a stdio JSON-RPC 2.0 sidecar daemon for the Dendrite Go control plane (`dendrite-control`), IDE extensions, or MCP servers (`dendrite-mcp-hub`).
- As a standalone CLI for developers to index, search, inspect symbol definitions/references, query the code graph, search git history, or evaluate configurations from the terminal.
- To execute the `ask` question-answering cascade combining syntax-aware retrieval with local, hybrid, or paid LLM tiers through a LiteLLM gateway.
- When project configuration needs to be managed via `.dendrite/rag.toml` files and hierarchical `DENDRITE_RAG_*` environment overrides.

### When not to use
- Do not use directly if you are writing a custom Rust application that needs an embedded, in-process retrieval engine without Dendrite configuration paths, CLI conventions, or RPC framing; use `loom-rag` directly instead.
- Do not use for general file quality linting and structural health gates; use the `loom-gate` CLI instead (or call the `quality.*` RPC methods exposed by `dendrite-rag serve`).
- Do not use to run statistical benchmark suites or paired bootstrap confidence interval comparisons against golden question sets; use `loom-eval` instead.

---

## Operating Modes

1. **JSON-RPC 2.0 server (default, or `serve`)**:
   Reads requests from stdin and writes responses to stdout, one JSON message per line. It keeps one open engine per project root for as long as the process runs. Logs go to stderr.

2. **Command-line interface (CLI)**:
   Subcommands to index, search, inspect symbols and the code graph, search git history, check index status and print settings from the terminal.

---

## CLI Command Reference

```bash
dendrite-rag [--json] [COMMAND]
```

`--json` is a global flag that prints JSON instead of text. Every subcommand except `serve` and `embedders` takes the project `<ROOT>` as its first argument.

| Command | Usage | Description |
|---|---|---|
| `serve` | `dendrite-rag serve` | Serves JSON-RPC 2.0 over stdio (the default when no subcommand is given). |
| `index` | `dendrite-rag index <ROOT> [--full] [--embed-budget-ms <MS>] [--embed-budget-tokens <N>] [--focus <QUESTION>]` | Indexes the project incrementally (BLAKE3 Merkle diff); `--full` rebuilds from scratch. The embedding budgets stop embedding after that many milliseconds or tokens and later runs continue; `--focus` embeds the chunks the question matches first when a budget applies. |
| `search` | `dendrite-rag search <ROOT> <QUERY> [-k <N>] [--mode <M>] [--corpus <C>] [--path <GLOB>]... [--min-score <S>]` | Searches the project. `--mode` is `hybrid` (default), `lexical` or `dense`; `--corpus` is `all` (default), `code` or `docs`; `-k`/`--top-k` defaults to 10; `--path` is repeatable. |
| `context` | `dendrite-rag context <ROOT> <QUERY> [--budget <TOKENS>] [--corpus <C>] [--detail <D>] [--docs-share <0..1>]` | Builds a token-budgeted context pack with citations. `--budget` defaults to 4000; `--corpus` as above; `--detail` is `auto` (default), `full` or `map`; `--docs-share` is the share of an `all` pack kept for documentation (default: `[ask] docs_share`, else 0.2). |
| `show` | `dendrite-rag show <ROOT> <CHUNK_ID>...` | Prints the full text of chunks by id (the bodies behind `context --detail map` outlines). |
| `symbols` | `dendrite-rag symbols <ROOT> <NAME> [--refs]` | Where a symbol is defined; with `--refs`, where it is referenced. |
| `grep` | `dendrite-rag grep <ROOT> <PATTERN> [--path <GLOB>]... [--max <N>]` | Regex search over indexable files. `--max` defaults to 200. |
| `history` | `dendrite-rag history <ROOT> <QUERY> [-k <N>]` | Searches commit messages and per-file diffs, indexing new commits first. `-k` defaults to 10. |
| `graph communities` | `dendrite-rag graph communities <ROOT> [--limit <N>] [--min-size <N>]` | Communities of files, largest first. Defaults: `--limit 50`, `--min-size 2`. |
| `graph god-nodes` | `dendrite-rag graph god-nodes <ROOT> [--min-in-degree <N>] [--limit <N>]` | Symbols with the most users. `--limit` defaults to 50. |
| `graph path` | `dendrite-rag graph path <ROOT> <FROM> <TO> [--kind <call\|import\|type>]... [--max-hops <N>]` | Shortest path between two symbols or files. `--max-hops` defaults to 6; without `--kind` all edge kinds are followed. |
| `graph explain` | `dendrite-rag graph explain <ROOT> <TARGET>` | Callers, callees, imports and community of a symbol or file. |
| `status` | `dendrite-rag status <ROOT>` | Index statistics: files, chunks, vectors, embedding model, embedding coverage and warnings when present, Merkle root, index directory, last indexed time. |
| `settings` | `dendrite-rag settings <ROOT>` | Prints the settings in effect: `.dendrite/rag.toml`, then environment overrides. |
| `embedders` | `dendrite-rag embedders` | Lists the embedding backends of this build with their default models. |
| `purge` | `dendrite-rag purge <ROOT>` | Deletes the project's index directory (`.dendrite/rag`). |
| `sync` | `dendrite-rag sync <ROOT>` | Sends commits journaled while the shared storage tier was unreachable (edge mode). |
| `push` | `dendrite-rag push <ROOT> [--dry-run]` | Uploads the index to the shared tier, records the commit and compacts the tier. Only in builds with the `postgres` feature. |
| `pull` | `dendrite-rag pull <ROOT>` | Fetches every missing embedding (from the shared tier in edge mode) for offline use. |

---

## Configuration Reference

Settings are read from `<ROOT>/.dendrite/rag.toml` (or the file named by `DENDRITE_RAG_CONFIG`). Without a file, defaults apply. `DENDRITE_RAG_*` environment variables override the file. Unknown keys, unknown backends and malformed values are errors.

### Example `.dendrite/rag.toml`

Values are examples unless a comment names a default.

```toml
[embed]
backend = "auto"                    # auto (default) | openai | ollama | hf | candle | oxillama | model2vec | none
model = "nomic-embed-text"          # each backend has a default model
url = "http://localhost:11434"      # HTTP backends only
# api_key_env = "HF_TOKEN"          # name of the variable holding the bearer key
# batch = 32                        # texts per request or forward pass
# dimensions = 1024                 # shorter vectors from an OpenAI-compatible server (Matryoshka models)
# budget_tokens = 50000000          # cap on estimated tokens embedded in one indexing run

[rerank]
# backend = "gateway"               # gateway (default) | candle | none
# model = "bge-reranker-v2-m3"      # the gateway reranker is off while model is unset
# abstain = 0.3                     # 0 to 1; off unless set (needs a reranker)

[index]
max_file_bytes = 1048576
query_cache = 256                   # query-vector LRU entries; 0 disables it
vector_index = "flat"               # auto | flat (int8 scan) | hnsw (needs the hnsw feature); auto uses flat
hnsw_ef_search = 128                # default 128

[fusion]                            # hybrid weights; unset keys keep the embedder's defaults
lexical = 1.0
dense = 1.0
symbol = 1.5
community = 0.0                     # boost for code in the best-matching communities

[lexical]                           # BM25
k1 = 1.2                            # term-frequency saturation (default 1.2)
b = 0.75                            # length normalization, 0 to 1 (default 0.75)
q = 1.0                             # q-logarithm of the IDF, (0, 1]; 1 is classic BM25 (default 1.0)

[storage]
mode = "local"                      # local (default) | remote | edge
ledger = "redb"                     # redb (default) | sqlite (needs the sqlite feature)
# repo = "github.com/acme/app"      # name in the shared tier; default: the origin URL

[graph]
resolution = 1.0                    # Leiden resolution: higher gives more, smaller communities

[graph.llm]
enabled = false
# model = "gpt-4o-mini"             # required when enabled
summaries = false
concepts = false

[ask]
local_model = "local-coder"         # default
paid_model = "gemini-2.5-pro"       # default
docs_share = 0.2                    # default; share of an `all` context pack kept for documentation (0 to 1)
```

Notes:

- `[embed] dimensions` is sent to the server as `dimensions` and is accepted only by the `openai` backend (or `auto`, which can resolve to the OpenAI-compatible gateway). Any other backend is a configuration error. The stored model id becomes `model@dims`.
- `[embed] budget_tokens` caps the embedding phase of one run. The run stops at the first group that would exceed it and reports `embed_pending`, `embed_tokens` and `budget_tokens_remaining`; the next run continues.
- `[rerank] abstain` is off unless set and needs a configured reranker. When the best reranked hit is less likely than the threshold to be relevant, `search` returns no hits, and `context` and `ask` return `abstained: true`.
- `[storage] ledger = "sqlite"` needs a build with the `sqlite` feature.

### Environment Variable Overrides

| Environment Variable | Overrides |
|---|---|
| `DENDRITE_RAG_CONFIG` | path of the settings file (default `<ROOT>/.dendrite/rag.toml`) |
| `DENDRITE_RAG_EMBED_BACKEND` | `embed.backend`. A backend different from the file's drops the file's `model`, `url`, prefixes and pooling |
| `DENDRITE_RAG_EMBED_MODEL`, `_URL`, `_API_KEY_ENV`, `_DOCUMENT_PREFIX`, `_QUERY_PREFIX`, `_POOLING`, `_BATCH`, `_DIMENSIONS`, `_BUDGET_TOKENS` | the matching `embed.*` key |
| `DENDRITE_RAG_RERANK_BACKEND`, `_MODEL`, `_URL`, `_API_KEY_ENV`, `_BATCH`, `_ABSTAIN` | the matching `rerank.*` key. A backend different from the file's drops the file's model and URL |
| `DENDRITE_RAG_MAX_FILE_BYTES`, `DENDRITE_RAG_QUERY_CACHE`, `DENDRITE_RAG_VECTOR_INDEX`, `DENDRITE_RAG_HNSW_EF_SEARCH` | `index.max_file_bytes`, `index.query_cache`, `index.vector_index`, `index.hnsw_ef_search` |
| `DENDRITE_RAG_FUSION_LEXICAL`, `_DENSE`, `_SYMBOL`, `_COMMUNITY` | `fusion.lexical`, `fusion.dense`, `fusion.symbol`, `fusion.community` |
| `DENDRITE_RAG_BM25_K1`, `_B`, `_Q` | `lexical.k1`, `lexical.b`, `lexical.q` |
| `DENDRITE_RAG_STORAGE_MODE`, `DENDRITE_RAG_STORAGE_REPO`, `DENDRITE_RAG_STORAGE_LEDGER` | `storage.mode`, `storage.repo`, `storage.ledger` (`redb`, `sqlite`) |
| `DENDRITE_RAG_PG_URL` | URL of the shared PostgreSQL tier; environment only, never read from the file |
| `DENDRITE_RAG_GRAPH_RESOLUTION` | `graph.resolution` |
| `DENDRITE_RAG_GRAPH_LLM_ENABLED`, `_MODEL`, `_SUMMARIES`, `_CONCEPTS` | `graph.llm.enabled`, `graph.llm.model`, `graph.llm.summaries`, `graph.llm.concepts` |
| `DENDRITE_RAG_ASK_LOCAL_MODEL`, `_PAID_MODEL`, `_DOCS_SHARE` | `ask.local_model`, `ask.paid_model`, `ask.docs_share` |

Empty variables are ignored.

---

## JSON-RPC 2.0 Protocol

The server reads newline-delimited JSON-RPC 2.0 from `stdin` and writes responses to `stdout`; logs go to `stderr`. Requests are handled one at a time in the order received. Full specification: [`docs/specs/protocol.md`](../../docs/specs/protocol.md); JSON Schemas are in [`proto/`](../../proto).

### Protocol version 1.12.0 and the Dendrite Go client

The protocol version is a semver string. `health` returns `{name: "dendrite-rag", version, protocol: "1.11.0", features}`. `features` lists the cargo features the binary was built with (`postgres`, `sqlite`, `hnsw`, `model2vec`, `candle`, `candle-metal`, `candle-cuda`, `candle-accelerate`, `oxillama`), so a client can tell, before opening a project, which `[embed]`, `[storage]` and `[rerank]` settings the binary can honour.

Dendrite's Go client (`pkg/sidecar.Client` in `dendrite-control`) spawns one sidecar process per call and runs the `health` handshake. It checks only the protocol **major** version: a server with a different major version is refused (and Dendrite falls back to its in-process Go implementation when the binary is missing, incompatible or unhealthy). Minor versions add optional params and result fields, and the client gates features on the minor version.

Minor additions to know about:

| Protocol | Added |
|---|---|
| 1.6 | `index` param `focus` |
| 1.7 | `docs_share` on `context` and `ask` |
| 1.8 | `abstained` in `context` and `ask` results |
| 1.9 | `index` param `embed_budget_tokens`; `index` result fields `embed_tokens` and `budget_tokens_remaining`; `features` in `health` |
| 1.10 | `quality.loc` method for production lines of code checking |
| 1.11 | `quality.clones`, `quality.complexity`, `quality.structure`, and `quality.rstest` methods |

Because the Go client starts a new process per call, nothing held in memory survives between calls. State that does: the index directory, the embedding cache, the answer cache, and the embedding backlog, which later `index` runs continue.

### Method Catalog

`root` is the project directory. Parameters marked `?` are optional.

| RPC Method | Params | Result |
|---|---|---|
| `health` | none | `{name, version, protocol, features}` |
| `index` | `{root, full?: false, embed_budget_ms?, embed_budget_tokens?, focus?}` | `{files_scanned, files_changed, files_removed, chunks_added, chunks_removed, embedded, embed_pending, embed_tokens, budget_tokens_remaining?, embed_error?, open_ms, walk_ms, graph_ms, embed_ms, duration_ms}`. With a budget, embedding stops when it runs out and later calls continue; the lexical index always completes. `focus` embeds the chunks the question matches first. `embed_error` is set when embedding failed; the lexical index is current. |
| `search` | `{root, query, top_k?: 10, mode?: "hybrid"\|"lexical"\|"dense", corpus?: "all"\|"code"\|"docs", paths?: [glob], min_score?, offset?: 0}` | `{hits, timings}` |
| `context` | `{root, query, token_budget?: 4000, corpus?, detail?: "auto"\|"full"\|"map", docs_share?: 0..1}` | `{spans, tokens, abstained?, timings}`. `abstained` appears only when `[rerank] abstain` is set and nothing was likely relevant. |
| `chunks.get` | `{root, ids: [chunk_id]}` | `{hits}`: full chunk text in the order asked; unknown ids are skipped. |
| `symbols.definitions` | `{root, name}` | `{hits}`: chunks defining the symbol. |
| `symbols.references` | `{root, name}` | `{hits}`: chunks calling or using the symbol. |
| `grep` | `{root, pattern, paths?, max_results?: 200}` | `{matches: [{path, line, text}]}` |
| `history.index` | `{root, max_commits?: 2000}` | `{commits, documents, head, rebuilt}` |
| `history.search` | `{root, query, top_k?: 10}` | `{hits}` |
| `graph.communities` | `{root, limit?: 50, min_size?: 2}` | `{communities}` |
| `graph.god_nodes` | `{root, min_in_degree?, limit?: 50}` | `{nodes}` |
| `graph.path` | `{root, from, to, kinds?: ["call","import","type"], max_hops?: 6}` | `{found, directed, level, steps}` |
| `graph.explain` | `{root, target}` | `{matches}` |
| `ask` | `{root, question, mode?: "local"\|"hybrid"\|"paid", top_k?, paths?, corpus?, docs_share?: 0..1, fresh?: false, local_model?, paid_model?, allow_paid?: false}` | `{answer, source_tier, model, local_draft?, escalation_needed, local_error?, escalated, prompt_tokens, output_tokens, hits, timings, cached, abstained?}`. `abstained` appears only when `[rerank] abstain` is set and a reranker found nothing likely relevant; no model is called and `answer` is empty. |
| `rank` | `{root, query, documents: [{id, text}], top_k?: 0}` | `{ranked}`: hybrid ranking of supplied documents; needs no index. |
| `status` | `{root}` | index status and statistics |
| `settings` | `{root}` | `{file, file_exists, settings}`: the settings after environment overrides |
| `embedders` | none | `{backends}` |
| `sync` | `{root}` | `{pending}` |
| `pull` | `{root}` | `{chunks, cached, vectors}` |
| `push` | `{root, dry_run?: false}` | upload and compaction report; only in builds with the `postgres` feature |
| `purge` | `{root}` | `{removed: true}` |
| `quality.loc` | `{root, config?, max_production_lines?: 500, warn_at?: 450, exclude?: [glob]}` | production lines of code report (`schema`, `totals`, `files`) |
| `quality.clones` | `{root, config?, max_percent?: 0.0, min_lines?: 5, min_tokens?: 50, include_tests?: true, exclude?: [glob]}` | structural clone duplication report (`schema`, `totals`, `groups`) |
| `quality.complexity` | `{root, config?, max_cyclomatic?: 10, max_cognitive?: 15, exclude?: [glob]}` | cyclomatic and cognitive complexity report (`schema`, `totals`, `findings`) |
| `quality.structure` | `{root, config?, max_files_per_dir?: 12, max_dir_production_lines?: 3000, max_prefix_group?: 4, exclude?: [glob]}` | module structure and directory size report (`schema`, `totals`, `findings`) |
| `quality.rstest` | `{root, exclude?: [glob]}` | Rust test conventions report (`schema`, `files`, `violations`) |

Errors use the standard JSON-RPC codes (`-32700`, `-32600`, `-32601`, `-32602`). Engine failures use `-32000` with `data.kind` (`config`, `not_indexed`, `io`, `embed`, `index`, or `git` for history methods). `search`, `context`, `ask`, `chunks.get`, `symbols.*` and `graph.*` return `not_indexed` before the first `index`.

---

## The `ask` Workflow

`ask` answers a question from the project. It calls models only through the LiteLLM gateway (`LITELLM_PROXY_URL`, `LITELLM_MASTER_KEY`). Details: [`docs/specs/ask.md`](../../docs/specs/ask.md).

1. **Context**: the context pack for the question (2,500 estimated tokens), or the top `top_k` hits. With `paths`, the top hits among those paths (4 unless `top_k` is set). Both come from the corpora in `corpus` (default `all`, which keeps `docs_share` of the budget for documentation).
2. **Abstention**: when `[rerank] abstain` is set and the reranker finds nothing likely to answer, no model is called and the result has `abstained: true`.
3. **Model**, by `mode`:
   - `local` (default): the local model (`local_model`, else `[ask] local_model`, else `local-coder`). An empty or canned answer counts as a failure.
   - `hybrid`: a local draft, then the paid model (`paid_model`, else `[ask] paid_model`, else `gemini-2.5-pro`) refines it; if refinement fails the draft is returned.
   - `paid`: the paid model directly.
4. **Escalation**: when the local model fails, the paid model is used only if `allow_paid` is set. Otherwise the result has `escalation_needed: true` and no answer, and the client asks the person before calling again with `allow_paid` or mode `paid`.
5. **Answer cache**: answers are cached in the project's ledger by model and exact prompt, so the same question about unchanged code returns without a model call (`cached: true`). `fresh: true` asks the model regardless.

The result reports the tier (`local-rag`, `hybrid`, `paid-cloud`), the model, the local draft (hybrid), token counts, the hits used and `timings`.

---

## Examples Guide

The crate provides two standalone examples in `examples/`:

### 1. `embed`
Prints the document vectors of the configured embedder for each argument, as JSON (`{"model": ..., "vectors": [...]}`). It reads settings from the current directory and fails with "backend is none" when the embedder is `none`.

```bash
cargo run --example embed -p dendrite-rag -- "fn main() {}" "struct Server"
```

With an in-process backend (for example Candle with Metal):
```bash
DENDRITE_RAG_EMBED_BACKEND=candle cargo run --release --features candle-metal --example embed -p dendrite-rag -- "vector search"
```

### 2. `eval`
Reports hit@1, hit@5, hit@10 and MRR@10 per mode (lexical, dense when vectors exist, hybrid), a subset for queries marked `"broad"`, latency with 8 concurrent readers, max RSS and int8-versus-exact vector recall, against a golden question set:

```bash
cargo run --example eval -p dendrite-rag -- . eval/golden-dendrite-rag.json
```

Arguments: `<root> <golden file> [index-dir]`. `EVAL_FULL=1` re-indexes from scratch (the embedding cache is kept). Measured results are in [`eval/RESULTS.md`](../../eval/RESULTS.md).

To sweep fusion weights (`EVAL_SWEEP=1` prints the best ten combinations, `EVAL_SWEEP=all` every one):
```bash
EVAL_SWEEP=1 cargo run --release --example eval -p dendrite-rag -- . eval/golden-dendrite-rag.json
```

---

## Library Usage

`dendrite-rag` also exposes a library interface (`dendrite_rag`) for embedding the sidecar server, resolving hierarchical `.dendrite/rag.toml` settings, or running `ask` programmatic workflows in Rust:

```rust
use std::path::Path;
use dendrite_rag::settings::resolve;
use loom_rag::engine::Engine;
use loom_rag::retrieve::{Mode, SearchOpts};

fn main() -> anyhow::Result<()> {
    let root = Path::new(".");

    // 1. Resolve configuration from .dendrite/rag.toml and environment overrides
    let config = resolve(root)?;

    // 2. Open the project engine using resolved settings
    let mut engine = Engine::open(root, config)?;
    let report = engine.index(false)?;
    println!(
        "Indexed {} files ({} changed) into {} chunks",
        report.files_scanned, report.files_changed, report.chunks_added
    );

    // 3. Search via hybrid retrieval
    let hits = engine.search(
        "reciprocal rank fusion",
        &SearchOpts {
            top_k: 5,
            mode: Mode::Hybrid,
            ..SearchOpts::default()
        },
    )?;
    for hit in hits {
        println!("{}:{} score: {:.4}", hit.path, hit.start_line, hit.score);
    }

    Ok(())
}
```

---

## Cargo Features

Feature flags forward directly to corresponding features in the [`loom-rag`](../loom-rag) engine library:

| Feature | Forwards To | Description |
|---|---|---|
| `postgres` | `loom-rag/postgres` | Enables PostgreSQL 16 + `pgvector` shared tier for remote storage and edge synchronization (`push`, `pull`, `sync`). |
| `sqlite` | `loom-rag/sqlite` | Enables SQLite bundled engine as an alternative local ledger (`[storage] ledger = "sqlite"`). |
| `hnsw` | `loom-rag/hnsw` | Enables Hierarchical Navigable Small World vector indexing. |
| `model2vec` | `loom-rag/model2vec` | Enables in-process static subword embeddings with `model2vec-rs` for offline use. |
| `candle` | `loom-rag/candle` | Enables in-process safetensors encoders (BERT, NomicBERT, JinaBERT) via Hugging Face Candle. |
| `candle-metal` | `loom-rag/candle-metal` | Enables Apple Silicon GPU acceleration via Metal for Candle embeddings. |
| `candle-cuda` | `loom-rag/candle-cuda` | Enables NVIDIA CUDA acceleration for Candle embeddings. |
| `candle-accelerate` | `loom-rag/candle-accelerate` | Enables Apple Accelerate CPU BLAS acceleration for Candle. |
| `oxillama` | `loom-rag/oxillama` | Enables in-process GGUF decoder embeddings (Qwen3-Embedding) via OxiLLaMa runtime. |

---

## Workspace & Sibling Repositories

- **`loom-rag`**: Core retrieval engine library consumed by `dendrite-rag`. Implements AST chunking, Tantivy BM25, vector search, Leiden community detection, and context packing without Dendrite dependencies.
- **`loom-gate`**: Quality gate CLI checking production lines per file, duplication, function complexity, package structure, and Rust rstest conventions. Shares quality verification algorithms with the `quality.*` RPC methods in `dendrite-rag`.
- **`loom-eval`**: Evaluation harness measuring Hit@k, MRR@10, nDCG@10, and token economics against golden sets and baselines.
- **`dendrite-core-rs`**: Sibling repository defining AST folding, token counting, and cyclomatic/cognitive complexity primitives.
- **`dendrite-git-rs`**: Sibling repository managing Git hunk dependency graphs and semantic review clustering.
- **`dendrite-plane-rs`**: Sibling repository providing Plane issue tracker integration.
- **`embedag`**: Rust library of graph building blocks (dependency ordering, Merkle delta trees, Graph-RAG primitives); `loom-rag` uses it for incremental change detection.
- **Dendrite control plane (`dendrite-control`)**: Consumes the `dendrite-rag` binary as a JSON-RPC 2.0 stdio sidecar for `dendrite ask`, `dendrite query`, and `dendrite store`. Falls back to in-process Go implementations if the sidecar binary is missing or unhealthy.
- **`dendrite-mcp-hub`**: Exposes MCP tools (`dendrite_rag_retrieve`, `dendrite_rag_ask`) to autonomous coding agents and IDE plugins.

---

## Building and Testing

Build the binary and library:
```bash
cargo build -p dendrite-rag
```

Run test suite:
```bash
cargo test -p dendrite-rag
```

Run tests with SQLite local ledger support:
```bash
cargo test -p dendrite-rag --features sqlite
```

---

## Specifications

- [System Architecture](../../docs/ARCHITECTURE.md): System design, component boundaries, and module layout.
- [Responsibilities & Ticket Map](../../docs/RESPONSIBILITIES.md): Ownership boundaries with `dendrite-control` and sibling repositories.
- [JSON-RPC Protocol Specification](../../docs/specs/protocol.md): JSON-RPC 2.0 stdio protocol catalog, parameters, and error codes.
- [Ask & Multi-Tier Workflow](../../docs/specs/ask.md): Prompt construction, local drafting, paid refinement, answer caching, and token ceilings.
- [Quality Gates Specification](../../docs/specs/quality.md): Verification algorithms for `quality.loc`, `quality.clones`, `quality.complexity`, `quality.structure`, and `quality.rstest`.
- [Embedding Backends](../../docs/specs/embedders.md): Embedding backend options, configuration hierarchy, and model defaults.
