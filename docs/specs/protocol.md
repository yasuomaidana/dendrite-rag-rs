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
| `status` | `{root}` | `{files, chunks, vectors, embed_model, merkle_root, index_dir, last_indexed}` |
| `purge` | `{root}` | `{removed: true}` |

`root` is the project directory. The server keeps one open engine per root.

Errors use the standard codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, and `-32602` invalid params (unknown root, bad mode, bad regex or glob). Engine failures use `-32000` with `data.kind`: `not_indexed` (`search`, `context` and `symbols.*` before the first `index`), `io`, `embed` (dense mode with the embedder unreachable; hybrid mode degrades to lexical + symbols instead), or `index`.

The same operations exist as dev subcommands, each with `--json`:
- `dendrite-rag index <root> [--full]`
- `search <root> <query> [-k N] [--mode M] [--path GLOB]… [--min-score S]`
- `context <root> <query> [--budget N]`
- `symbols <root> <name> [--refs]`
- `grep <root> <pattern> [--path GLOB]… [--max N]`
- `status <root>`
- `purge <root>`

## Configuration

Read from the environment at start-up:

| Variable | Meaning | Default |
|---|---|---|
| `DENDRITE_RAG_EMBED_BACKEND` | `gateway`, `model2vec` (needs the `model2vec` build feature) or `none` | `gateway` |
| `DENDRITE_RAG_EMBED_MODEL` | Embedding model | `local-nomic-embed-text-latest` / `minishlab/potion-code-16M-v2` |
| `DENDRITE_RAG_RERANK_MODEL` | Cross-encoder served at the gateway's `/v1/rerank`; reranking is off when unset | — |
| `DENDRITE_RAG_MAX_FILE_BYTES` | Largest file indexed | 1 MiB |
| `LITELLM_PROXY_URL` | Gateway base URL | `http://localhost:4000` |
| `LITELLM_API_KEY`, `LITELLM_MASTER_KEY`, `OPENAI_API_KEY` | Gateway key, first one set wins | — |
