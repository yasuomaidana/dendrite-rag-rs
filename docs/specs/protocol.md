# `dendrite-rag` JSON-RPC protocol

`dendrite-rag serve` (the default when no subcommand is given) speaks JSON-RPC 2.0 over stdio, one JSON message per line on stdin and stdout. Logs go to stderr. The protocol version is a semver string. Clients must reject a server whose major version differs from the one they were built for.

JSON Schemas for every method's params and result live in [`proto/`](../../proto).

Protocol version: **1.11.0** (1.1 added the `graph.*` methods and the `communities` fields of `index` and `status`; 1.2 added `sync`, `pull`, `push` and `status.sync_pending`; 1.3 added `rank`; 1.4 added `ask`; 1.5 added `corpus` to `search`, `context` and `ask`, `timings` to their results, and `open_ms`, `walk_ms` and `embed_ms` to `index`; 1.6 added `context.detail`, `chunks.get`, `index.focus`, the hit fields `duplicates` and `outline`, `status.embed_coverage` and `status.warnings`, and `ask.fresh` with `cached` answers; 1.7 added `docs_share` to `context` and `ask`; 1.8 added `abstained` to `context` and `ask` results, set when `[rerank] abstain` finds nothing likely relevant; 1.9 added `embed_budget_tokens` to `index`, `embed_tokens` and `budget_tokens_remaining` to its result, and `features` to `health`; 1.10 added `quality.loc`; 1.11 added `quality.clones` and `quality.rstest`)

| Method | Params | Result |
|---|---|---|
| `health` | — | `{name: "dendrite-rag", version, protocol: "1.11.0", features: [...]}`; `features` lists the cargo features the binary was built with (protocol 1.9) |
| `index` | `{root, full?: false, embed_budget_ms?, embed_budget_tokens?, focus?}` | `{files_scanned, files_changed, files_removed, chunks_added, chunks_removed, embedded, embed_pending, embed_tokens, budget_tokens_remaining?, embed_error?, open_ms, walk_ms, graph_ms, embed_ms, duration_ms}`; with a budget, embedding stops when it runs out and later calls continue; `focus` (the question about to be asked) embeds the chunks it matches first ([retrieval.md](retrieval.md) § Embedding backlog) |
| `search` | `{root, query, top_k?: 10, mode?: "hybrid"\|"lexical"\|"dense", corpus?: "all"\|"code"\|"docs", paths?: [glob], min_score?, offset?: 0}` | `{hits: [{chunk_id, path, start_line, end_line, scope, score, content, duplicates?, outline?}], timings}` |
| `context` | `{root, query, token_budget?: 4000, corpus?: "all"\|"code"\|"docs", detail?: "auto"\|"full"\|"map", docs_share?: 0..1}` | `{spans: [hit...], tokens, abstained?, timings}` ([retrieval.md](retrieval.md) § Corpora and § Context packing) |
| `chunks.get` | `{root, ids: [chunk_id]}` | `{hits}`: the full text of chunks, in the order asked (the bodies behind outlines); unknown ids are skipped |
| `symbols.definitions` | `{root, name}` | `{hits}`: chunks defining `name` |
| `symbols.references` | `{root, name}` | `{hits}`: chunks calling or using `name` |
| `ask` | `{root, question, mode?: "local"\|"hybrid"\|"paid", top_k?, corpus?: "all", docs_share?: 0..1, fresh?: false, local_model?, paid_model?, allow_paid?: false}` | `{answer, source_tier, model, local_draft?, escalation_needed, local_error?, escalated, prompt_tokens, output_tokens, hits, timings, cached, abstained?}` ([ask.md](ask.md)); `abstained` is set when `[rerank] abstain` found nothing likely relevant: no model was called and `answer` is empty |
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
| `status` | `{root}` | `{files, chunks, vectors, embed_model, embed_coverage?, warnings?, merkle_root, index_dir, last_indexed}` |
| `quality.loc` | `{root, config?, max_production_lines?: 500, warn_at?: 450, exclude?: [glob]}` | the report of `loom-gate loc`: `{schema, max_production_lines, warn_at, totals: {files, production, test, warnings, failures}, files: [{path, language, production, test, comment, blank, status: "ok"\|"warn"\|"fail", parse_errors}]}`; limits come from the `[loc]` table of `config` (default `<root>/.github/dendrite/directives.toml`), then the params ([quality.md](quality.md)); no index needed |
| `quality.clones` | `{root, config?, max_percent?: 0, min_lines?: 5, min_tokens?: 50, include_tests?: true, exclude?: [glob]}` | the report of `loom-gate clones`: `{schema, max_percent, min_lines, min_tokens, include_tests, totals: {files, lines, duplicated_lines, percent, groups}, groups: [{id, tokens, locations: [{path, start_line, end_line}]}]}`; limits come from the `[duplication]` table of `config` (default `<root>/.github/dendrite/directives.toml`), then the params ([quality.md](quality.md)); no index needed |
| `quality.rstest` | `{root, exclude?: [glob]}` | the report of `loom-gate rstest`: `{schema, files, violations: [{kind: "repeated_test"\|"helper_should_be_fixture", path, line, message}]}` |
| `purge` | `{root}` | `{removed: true}` |
| `embedders` | — | `{backends: [{backend, compiled, default_model, default_url, description}]}` |
| `settings` | `{root}` | `{file, file_exists, settings: {embed, rerank, index, graph, fusion, lexical, storage, ask}}`: the project's settings after environment overrides (keys are never returned) |

`root` is the project directory. The server keeps one open engine per root.

Errors use the standard codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, and `-32602` invalid params (unknown root, bad mode, bad regex or glob). Engine failures use `-32000` with `data.kind`: `config` (the project's `.dendrite/rag.toml` or a `DENDRITE_RAG_*` variable is invalid), `not_indexed` (`search`, `context`, `symbols.*` and `graph.*` before the first `index`), `io`, `embed` (dense mode with the embedder unreachable; hybrid mode degrades to lexical + symbols instead), or `index`. History methods use `git` for repository errors.

The same operations exist as dev subcommands, each with `--json`:
- `dendrite-rag index <root> [--full] [--embed-budget-ms N] [--focus QUESTION]`
- `search <root> <query> [-k N] [--mode M] [--corpus C] [--path GLOB]… [--min-score S]`
- `context <root> <query> [--budget N] [--corpus C] [--detail D]`
- `show <root> <chunk_id>…`
- `symbols <root> <name> [--refs]`
- `grep <root> <pattern> [--path GLOB]… [--max N]`
- `history <root> <query> [-k N]`
- `status <root>`
- `purge <root>`
- `embedders`
- `settings <root>`

## Configuration

Each project is configured by its `.dendrite/rag.toml`, overridden by `DENDRITE_RAG_*` environment variables; the server resolves the settings when it first opens a root. [embedders.md](embedders.md) lists every key and variable. `LITELLM_PROXY_URL`, `OLLAMA_HOST` and the provider keys (`LITELLM_API_KEY`, `HF_TOKEN`, …) are read as the defaults of their backends.
