# `dendrite-rag` JSON-RPC protocol

`dendrite-rag serve` (the default when no subcommand is given) speaks JSON-RPC 2.0 over stdio, one JSON message per line on stdin and stdout. Logs go to stderr. The protocol version is a semver string. Clients must reject a server whose major version differs from the one they were built for.

JSON Schemas for every method's params and result live in [`proto/`](../../proto).

Protocol version: **1.3.0** (1.1 added the `graph.*` methods and the `communities` fields of `index` and `status`; 1.2 added `sync`, `pull`, `push` and `status.sync_pending`; 1.3 added `rank`)

| Method | Params | Result |
|---|---|---|
| `health` | — | `{name: "dendrite-rag", version, protocol: "1.3.0"}` |
| `index` | `{root, full?: false, embed_budget_ms?}` | `{files_scanned, files_changed, files_removed, chunks_added, chunks_removed, embedded, embed_pending, embed_error?, duration_ms}`; with a budget, embedding stops when it runs out and later calls continue |
| `search` | `{root, query, top_k?: 10, mode?: "hybrid"\|"lexical"\|"dense", paths?: [glob], min_score?, offset?: 0}` | `{hits: [{chunk_id, path, start_line, end_line, scope, score, content}]}` |
| `context` | `{root, query, token_budget?: 4000}` | `{spans: [hit...], tokens}` |
| `symbols.definitions` | `{root, name}` | `{hits}`: chunks defining `name` |
| `symbols.references` | `{root, name}` | `{hits}`: chunks calling or using `name` |
| `rank` | `{root, query, documents: [{id, text}], top_k?: 0}` | `{ranked: [{id, score, lexical, dense?}]}`: hybrid ranking of supplied documents, no index needed ([retrieval.md](retrieval.md) § Ranking supplied documents) |
| `graph.communities` | `{root, limit?: 50, min_size?: 2}` | `{communities: [{id, label, size, files, top_symbols, summary}]}`, largest first |
| `graph.god_nodes` | `{root, min_in_degree?: 21, limit?: 50}` | `{nodes: [{symbol, path, line, in_degree, out_degree, used_from_files, community, summary}]}` |
| `graph.path` | `{root, from, to, kinds?: ["call","import","type"], max_hops?: 6}` | `{found, directed, level: "symbol"\|"file", steps: [{node, location?, via?, reversed?}]}` |
| `graph.explain` | `{root, target}` | `{matches: [{kind: "symbol", …} \| {kind: "file", …}]}`: callers, callees, imports, community ([graph.md](graph.md)) |
| `grep` | `{root, pattern, paths?, max_results?: 200}` | `{matches: [{path, line, text}]}` (no index needed) |
| `history.index` | `{root, max_commits?: 2000}` | `{commits, documents, head, rebuilt}` |
| `history.search` | `{root, query, top_k?: 10}` | `{hits: [{sha, time, author, subject, path, diff, score}]}`; updates the history index first ([history.md](history.md)) |
| `sync` | `{root}` | `{pending}`: sends commits journaled while the shared tier was unreachable (edge mode) |
| `push` | `{root, dry_run?: false}` | `{dry_run, commit, branch, files, chunks, embeddings_uploaded, vectors, compaction}`: uploads to the shared tier and compacts it (feature `postgres`; [store.md](store.md) § Push and compaction) |
| `pull` | `{root}` | `{chunks, cached, vectors}`: fetches missing embeddings (from the shared tier in edge mode) for offline use |
| `status` | `{root}` | `{files, chunks, vectors, embed_model, merkle_root, index_dir, last_indexed}` |
| `purge` | `{root}` | `{removed: true}` |
| `embedders` | — | `{backends: [{backend, compiled, default_model, default_url, description}]}` |
| `settings` | `{root}` | `{file, file_exists, settings: {embed, rerank, index}}`: the project's settings after environment overrides (keys are never returned) |

`root` is the project directory. The server keeps one open engine per root.

Errors use the standard codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, and `-32602` invalid params (unknown root, bad mode, bad regex or glob). Engine failures use `-32000` with `data.kind`: `config` (the project's `.dendrite/rag.toml` or a `DENDRITE_RAG_*` variable is invalid), `not_indexed` (`search`, `context`, `symbols.*` and `graph.*` before the first `index`), `io`, `embed` (dense mode with the embedder unreachable; hybrid mode degrades to lexical + symbols instead), or `index`. History methods use `git` for repository errors.

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
