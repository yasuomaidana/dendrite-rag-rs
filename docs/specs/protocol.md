# `dendrite-rag` JSON-RPC protocol

`dendrite-rag serve` (the default when no subcommand is given) speaks JSON-RPC 2.0 over stdio, one JSON message per line on stdin and stdout. Logs go to stderr. The protocol version is a semver string. Clients must reject a server whose major version differs from the one they were built for.

JSON Schemas for every method's params and result live in [`proto/`](../../proto).

Protocol version: **1.0.0**

| Method | Params | Result |
|---|---|---|
| `health` | — | `{name: "dendrite-rag", version, protocol: "1.0.0"}` |
| `index` | `{root, full?: bool}` | `{files_scanned, files_changed, files_removed, chunks_added, chunks_removed, embedded, duration_ms}` |
| `search` | `{root, query, top_k?: 10, mode?: "hybrid"\|"lexical"\|"dense", paths?: [glob]}` | `{hits: [{chunk_id, path, start_line, end_line, scope, score, content}]}` |
| `context` | `{root, query, token_budget?: 4000}` | `{spans: [hit...], tokens}` |
| `status` | `{root}` | `{files, chunks, vectors, embed_model, index_dir, last_indexed}` |
| `purge` | `{root}` | `{removed: bool}` |

`root` is the project directory. The server keeps one open engine per root.

Errors use the standard codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, `-32602` invalid params, and `-32000` engine error (with `data.kind`: `not_indexed`, `io`, `embed`, `index`).

The same operations exist as dev subcommands: `dendrite-rag index <root> [--full]`, `search <root> <query> [-k N] [--mode M]`, `context <root> <query> [--budget N]`, `status <root>`, `purge <root>`.
