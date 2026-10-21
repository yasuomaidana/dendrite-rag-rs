# `dendrite-rag` JSON-RPC protocol

`dendrite-rag serve` (the default when no subcommand is given) speaks JSON-RPC 2.0 over stdio, one JSON message per line on stdin and stdout. Logs go to stderr. The protocol version is a semver string. Clients must reject a server whose major version differs from the one they were built for.

JSON Schemas for every method's params and result live in [`proto/`](../../proto).

Protocol version: **1.0.0**

| Method | Params | Result |
|---|---|---|
| `health` | — | `{name: "dendrite-rag", version, protocol: "1.0.0"}` |
| `index` | `{root, full?: false, embed_budget_ms?}` | `{files_scanned, files_changed, files_removed, chunks_added, chunks_removed, embedded, embed_pending, embed_error?, duration_ms}`; with a budget, embedding stops when it runs out and later calls continue |
| `search` | `{root, query, top_k?: 10, mode?: "hybrid"\|"lexical"\|"dense", paths?: [glob], min_score?, offset?: 0}` | `{hits: [{chunk_id, path, start_line, end_line, scope, score, content}]}` |
| `context` | `{root, query, token_budget?: 4000}` | `{spans: [hit...], tokens}` |
| `symbols.definitions` | `{root, name}` | `{hits}`: chunks defining `name` |
| `symbols.references` | `{root, name}` | `{hits}`: chunks calling or using `name` |
| `grep` | `{root, pattern, paths?, max_results?: 200}` | `{matches: [{path, line, text}]}` (no index needed) |
| `history.index` | `{root, max_commits?: 2000}` | `{commits, documents, head, rebuilt}` |
| `history.search` | `{root, query, top_k?: 10}` | `{hits: [{sha, time, author, subject, path, diff, score}]}`; updates the history index first ([history.md](history.md)) |
| `status` | `{root}` | `{files, chunks, vectors, embed_model, merkle_root, index_dir, last_indexed}` |
| `purge` | `{root}` | `{removed: true}` |
| `embedders` | — | `{backends: [{backend, compiled, default_model, default_url, description}]}` |
| `settings` | `{root}` | `{file, file_exists, settings: {embed, rerank, index}}`: the project's settings after environment overrides (keys are never returned) |

`root` is the project directory. The server keeps one open engine per root.

Errors use the standard codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, and `-32602` invalid params (unknown root, bad mode, bad regex or glob). Engine failures use `-32000` with `data.kind`: `config` (the project's `.dendrite/rag.toml` or a `DENDRITE_RAG_*` variable is invalid), `not_indexed` (`search`, `context` and `symbols.*` before the first `index`), `io`, `embed` (dense mode with the embedder unreachable; hybrid mode degrades to lexical + symbols instead), or `index`. History methods use `git` for repository errors.

The same operations exist as dev subcommands, each with `--json`:
- `dendrite-rag index <root> [--full]`
- `search <root> <query> [-k N] [--mode M] [--path GLOB]… [--min-score S]`
- `context <root> <query> [--budget N]`
- `symbols <root> <name> [--refs]`
- `grep <root> <pattern> [--path GLOB]… [--max N]`
- `history <root> <query> [-k N]`
- `status <root>`
- `purge <root>`
- `embedders`
- `settings <root>`

## Configuration

Each project is configured by its `.dendrite/rag.toml`, overridden by `DENDRITE_RAG_*` environment variables; the server resolves the settings when it first opens a root. [embedders.md](embedders.md) lists every key and variable. `LITELLM_PROXY_URL`, `OLLAMA_HOST` and the provider keys (`LITELLM_API_KEY`, `HF_TOKEN`, …) are read as the defaults of their backends.
