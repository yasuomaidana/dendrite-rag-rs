# Code graph and communities

The code graph answers "how is this codebase organised, and what connects to what?", the question FEAT-045 (Graphify) asks. It is derived from what the chunker already extracts, so it needs no second parser, no LLM and no Python runtime.

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
