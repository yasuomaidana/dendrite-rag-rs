# Code graph and communities

The code graph answers "how is this codebase organised, and what connects to what?", the question FEAT-045 (Graphify, DEND-201/202) asks. It is derived from what the chunker already extracts, so it needs no second parser, no LLM and no Python runtime.

## Edges

| Kind | From → to | Source | Weight |
|---|---|---|---|
| `call` | caller → definition | `@reference.call` tags (`Chunk.calls`) | 1.0 |
| `import` | importing file → imported file | per-language import queries (`FileEntry.imports`) | 0.6 |
| `type` | user → type definition | other `@reference.*` tags (`Chunk.refs` minus `calls`) | 0.3 |

The stock tag queries are extended with calls for C and C++, whose grammars ship definitions only.

**Import queries** capture the module specifier as written: Go `import_spec`, Rust `use_declaration` and `mod name;` (reported as `mod:name`), Python `import` / `from … import`, JavaScript and TypeScript `import`, `export … from` and `require("…")`, Java `import_declaration`, C and C++ `#include`.

**Resolution** (`src/graph/resolve.rs`) maps a specifier to project files, best effort:

| Language | Rule |
|---|---|
| Go | module path from every `go.mod` (the longest prefix wins), then all non-test `.go` files of the package directory |
| Rust | `crate::`, `self::` and `super::` paths; the longest existing module prefix as `x.rs` or `x/mod.rs`; `mod name;` as a child module |
| Python | relative imports from the file's package; absolute ones from the root, `src/` or `lib/`, else a unique suffix match |
| JS / TS | relative specifiers with the usual extensions, `.js` → `.ts`, and `index.*` |
| Java | the class file by package path; `pkg.*` as every file of that package |
| C / C++ | relative to the including file, else a unique suffix match |

A specifier that names no project file (the standard library, a package) is kept as an *external* import of the file.

## Linking names to definitions

Tags carry bare names, so a use is linked only to definitions it can see:

- definitions in the same directory (package or module) or in a file the user imports;
- Go cannot use another package's names without importing it, so a Go use is never linked further;
- in other languages an unambiguous definition (at most two) is accepted when none is visible, because methods are called on values whose type is not imported;
- a receiver call (`x.len()`, `obj.close()`) links only when the project defines that name once and it is not a standard container, string or iterator method (`len`, `push`, `get`, …);
- code never links to definitions in test files unless it is test code itself;
- names with more than 8 visible definitions are skipped.

Symbol nodes are `(name, defining file)`: `Client` in two packages is two nodes. A chunk without definitions (a piece of a large function) is attributed to its innermost scope.

## Graph levels

- **Files** carry directed, typed, weighted edges. Communities are computed on the symmetric file graph whose edge weight sums every kind in both directions.
- **Symbols** carry directed `call` and `type` edges from user to definition. In-degree (distinct users) identifies god nodes; FEAT-045 uses in-degree > 20.

The graph is rebuilt in memory from the ledger on first use after an index change: about 50 ms for the Dendrite repository (833 files, 5,462 symbol nodes). `examples/graph_stats.rs` prints the counts and the most-used symbols.

## Communities

Files are partitioned with **Leiden** (`src/graph/leiden.rs`, pure Rust) maximizing modularity with resolution γ (`[graph] resolution`, `DENDRITE_RAG_GRAPH_RESOLUTION`, default 1.0; higher gives more, smaller communities) on the symmetric file graph. Each level runs fast local moving, refines every community so that only well-connected subsets merge (communities are always connected), then aggregates. Refinement takes the best merge instead of a random one, so the partition is deterministic for a given set of files.

- **When:** after every index run that changed files, and once for a ledger that has none. On the Dendrite repository it takes about 0.15–0.3 s including the graph build. `index` reports `communities` and `graph_ms`; `status` reports `communities`.
- **Stable ids:** new communities, largest first, take the free previous id they share the most files with, so ids, labels and citations do not churn when one file changes.
- **Label:** the directory holding most members, then the most-used symbol: `pkg/synapse · Member`.
- **Record:** id, label, member files and the 10 most-used symbols (`name`, `path`, `line`, `in_degree`).
- Files with no edges (most docs and config) are communities of one.

| Ledger table (redb / sqlite) | Key | Value |
|---|---|---|
| `community_files` | path | community id |
| `community_chunks` | chunk id | community id (each chunk inherits its file's) |
| `communities` | community id | the record, JSON |

The local ledger uses redb by default, with SQLite opt-in (`[storage] ledger = "sqlite"`, feature `sqlite`, DRAG-49); both implement the same tables.

## Queries

Served as JSON-RPC methods ([protocol.md](protocol.md)) and as `dendrite-rag graph <op>` subcommands; each answers from the in-memory graph and the stored communities, in well under the FEAT-045 limit of 2 s (1.5 s for a cold CLI run on the Dendrite repository, index check included).

| Method | Answers |
|---|---|
| `graph.communities` | Communities with at least `min_size` files, largest first, with their label, files, most-used symbols and a one-paragraph summary |
| `graph.god_nodes` | Symbols with at least `min_in_degree` users (default 21: FEAT-045's "more than 20"), with the number of files they are used from, their community and a summary |
| `graph.path` | The shortest path between two symbols over call and type edges, or between two files (a symbol stands for its defining files) over every edge kind; `kinds` restricts the edges. A forward path is preferred; otherwise edges may be followed backwards, marked `reversed` |
| `graph.explain` | For a symbol: its definitions, users, uses, community and god-node flag. For a file: what it defines, imports and is imported by, external imports, the files it depends on and those that use it |

## Summary chunks

Every community of two or more files gets a summary chunk (`src/graph/summary.rs`): its label, files, most-used symbols and a one-paragraph description, all deterministic (an optional LLM summary is supported via DRAG-36, § Optional LLM layer). Its id and path start with `@community/`, which no project file can; the id carries the content hash, so a changed summary is a new chunk and its old vector is dropped. Summaries are written with the communities: lexical index first, then the ledger (replacing the last run's), and embedded with the other chunks.

In search they never appear and never compete with code: hybrid search splits them out of the lexical and dense rankings and fuses them into a separate summary ranking. Code that the other rankings found inside the two best-matching communities can be raised by `community` (`[fusion] community` / `DENDRITE_RAG_FUSION_COMMUNITY`, formerly `COMMUNITY_BOOST`); it is 0 (off) by default, because on the golden sets it did not help (eval/RESULTS.md, DRAG-31), and DRAG-26 makes it tunable per corpus.

In context packs, a neighbour pulled in through the symbol graph keeps `HOP_DECAY` when it is in the same community as its hit and decays once more (×0.5) when it is not.

## Optional LLM layer

The graph layer supports an optional LLM enrichment phase (`[graph.llm]` in `.dendrite/rag.toml`, DRAG-36):
- **Off by default**: `enabled = false, summaries = false, concepts = false`. When disabled, the entire graph pipeline remains completely deterministic with zero network calls and identical eval numbers.
- **Gateway client**: A small blocking client (`src/llm.rs`) communicating with an OpenAI-compatible LiteLLM proxy via `POST {url}/v1/chat/completions`. URL is read from `LITELLM_PROXY_URL` (default `http://localhost:4000`), bearer key from `LITELLM_MASTER_KEY` then `LITELLM_API_KEY`. Always runs at temperature 0 with a timeout and JSON mode support. Failures yield typed errors and never panic.
- **Community summaries**: When `summaries = true`, the model generates a 3-5 sentence summary from the deterministic community summary text plus the first lines of its top-symbol chunks. This description is appended to the summary chunk, updating its content and hash.
- **Concepts**: When `concepts = true`, the model extracts up to 8 key concepts as JSON `[{name, description}]` per prose file (Markdown, `.txt`, `.rst`; the first 12,000 characters), not the configuration and data files that are also indexed as text. Extracted concepts are stored in the ledger table `concepts` (`path -> JSON list`) and exposed read-only in `graph.explain` for a file (`concepts` field, omitted when empty) and in `proto/graph.explain.json`. Images are out of scope for now.
- **Caching & cost control**: Every model response is cached in the ledger table `llm_cache` (redb or SQLite) keyed by `(model, BLAKE3 of prompt)` through `LedgerStore` and `LedgerWrite`. Unchanged communities and files never call the model again. The `llm_cache` table is preserved across index rebuilds. If a model call fails, the deterministic summary is kept, indexing succeeds, the error is reported in `IndexReport` under `llm_error`, and the gateway is not asked again in that run (only cached answers are used), so a gateway outage costs one timeout, not one per community or file.
- **What is sent**: community labels, file paths, symbol names and their first lines, and the text of prose files, to the gateway's model only. Nothing is sent unless `enabled` and `summaries` or `concepts` are set.
