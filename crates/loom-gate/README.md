# loom-gate

[![Crate](https://img.shields.io/badge/crate-loom--gate-blue.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](../../Cargo.toml)

`loom-gate` is a command-line tool that evaluates source code trees against project quality directives. Built for continuous integration pipelines and autonomous developer agents, it checks five code-health gates: production lines of code per file (`loc`), structural code duplication (`clones`), cyclomatic and cognitive function complexity (`complexity`), directory organization and module grouping (`structure`), and Rust test organization (`rstest`).

---

## When to Use It

### When to use
- In CI pipelines (such as GitHub Actions) and local pre-commit hooks to enforce file size limits, complexity ceilings, and duplication thresholds.
- Inside autonomous developer agent verification steps to ensure generated or modified code does not breach architectural directives.
- To produce structured machine-readable reports (`--format json`) for tooling integration, or workflow command annotations (`--format github`) for pull request status checks.

### When not to use
- Do not use as a linter for syntax, style, or idioms; use `cargo clippy`, `eslint`, or `ruff` instead.
- Do not use as a code formatter; use `cargo fmt`, `prettier`, or `black`.
- Do not use to track code coverage percentage; use dedicated coverage tools such as `cargo llvm-cov`.
- When integrating with Dendrite through JSON-RPC 2.0 daemon mode over stdio, invoke the `quality.*` RPC methods via `dendrite-rag serve` rather than spawning the CLI directly.

---

## Exit Codes

| Exit Code | Meaning |
|---|---|
| `0` | The gate passed. Warnings may be emitted, but no failure thresholds were exceeded. |
| `1` | The gate failed. At least one file, function, or directory breached configured limits. |
| `2` | The tool could not run due to invalid CLI arguments, an unreadable or malformed directives configuration file, or contradictory configuration limits (such as `warn_at` exceeding `max_production_lines`). |

---

## Configuration

`loom-gate` reads directives from `<root>/.github/dendrite/directives.toml` by default, or from a path passed via `--config <PATH>`. Command-line arguments override directives file settings. If a directives file or table is missing, built-in defaults apply.

Example `.github/dendrite/directives.toml`:

```toml
schema = 1

[loc]
max_production_lines = 500
warn_at = 450
exclude = ["**/generated/**"]

[duplication]
max_percent = 0.0
min_tokens = 50
min_lines = 5
include_tests = true

[complexity]
max_cyclomatic = 10
max_cognitive = 15
exclude = []

[structure]
max_files_per_dir = 12
max_dir_production_lines = 3000
max_prefix_group = 4
require_docs = true
docs_files = ["README.md", "CHANGELOG.md"]
exclude = []
```

---

## Subcommands

Every subcommand accepts standard common flags:
- `--root <PATH>`: Project root directory (default: `.`).
- `--config <PATH>`: Directives file path (default: `<root>/.github/dendrite/directives.toml`).
- `--format <text|json|github>`: Output format (`text` default, `json`, or `github`).
- `--exclude <GLOB>`: Glob pattern relative to root for files not checked (repeatable).

### 1. `loc` (Production Lines of Code)

Checks that individual code files do not exceed the configured maximum number of production lines. Production lines are lines containing at least one non-blank character outside comments and outside test code. Tests, comments, and doc comments are stripped using Tree-sitter AST queries, so inline test modules (`#[cfg(test)] mod tests`) do not count against file limits.

```bash
loom-gate loc [--root .] [--config <PATH>] [--format text|json|github] [--max N] [--warn-at N] [--exclude GLOB]...
```

**Options:**
- `--max <N>`: Maximum allowable production lines per file (overrides `loc.max_production_lines`, default: 500).
- `--warn-at <N>`: Production lines threshold at which a warning is issued (overrides `loc.warn_at`, default: 450).

**Example:**
```bash
cargo run -p loom-gate -- loc --root . --max 500 --warn-at 450
```

### 2. `clones` (Code Duplication)

Identifies repeated blocks of code across the repository. Two syntax subtrees match when they are structurally identical after identifiers and literals are replaced by wildcards, capturing copy-pasted code with renamed variables or changed constants.

```bash
loom-gate clones [--root .] [--config <PATH>] [--format text|json|github] [--max-percent P] [--min-lines N] [--min-tokens N] [--include-tests BOOL] [--exclude GLOB]...
```

**Options:**
- `--max-percent <P>`: Maximum allowed percentage of duplicated lines across the codebase (default: 0.0).
- `--min-lines <N>`: Minimum line length for a repeated block to count (default: 5).
- `--min-tokens <N>`: Minimum syntax tree leaf tokens required for a block to count (default: 50).
- `--include-tests <BOOL>`: Whether to check duplication in test code and test files (default: true).

**Example:**
```bash
cargo run -p loom-gate -- clones --root . --min-tokens 50 --min-lines 5
```

### 3. `complexity` (Function Complexity)

Fails when any function exceeds McCabe cyclomatic or SonarSource cognitive complexity thresholds. Complexity is evaluated across Go, Rust, Python, TypeScript, TSX, JavaScript, Java, Kotlin, Svelte (the functions of its scripts), C, and C++.

```bash
loom-gate complexity [--root .] [--config <PATH>] [--format text|json|github] [--max-cyclomatic N] [--max-cognitive N] [--exclude GLOB]...
```

**Options:**
- `--max-cyclomatic <N>`: Maximum McCabe cyclomatic complexity per function (default: 10).
- `--max-cognitive <N>`: Maximum SonarSource cognitive complexity per function (default: 15).

**Example:**
```bash
cargo run -p loom-gate -- complexity --root . --max-cyclomatic 10 --max-cognitive 15
```

### 4. `structure` (Directory and Module Organization)

Enforces package and directory modularity rules to prevent unstructured accumulation of files. Tests associated with a module (`x_test.go`, `x_tests.rs`, `test_x.py`) count toward their parent module rather than as standalone files.

```bash
loom-gate structure [--root .] [--config <PATH>] [--format text|json|github] [--max-files N] [--max-lines N] [--max-prefix-group N] [--exclude GLOB]...
```

**Options:**
- `--max-files <N>`: Maximum number of modules allowed in a single directory (default: 12).
- `--max-lines <N>`: Maximum cumulative production lines allowed across files in a directory (default: 3000).
- `--max-prefix-group <N>`: Maximum number of module files allowed to share the same prefix word before requiring a sub-module directory (default: 4).

A fourth rule, `docs`, requires every project unit to carry its documentation: the repository root and each directory with a Cargo package, `go.mod`, `package.json` or `pyproject.toml` need the files named in `structure.docs_files` (default `README.md` and `CHANGELOG.md`). Turn it off with `structure.require_docs = false`. A `docs` finding has `limit` 0 and `value` equal to the number of missing files.

**Example:**
```bash
cargo run -p loom-gate -- structure --root . --max-files 12 --max-lines 3000 --max-prefix-group 4
```

### 5. `rstest` (Rust Test Organization)

Validates Rust test code to ensure idiomatic structure:
- `repeated_test`: Detects plain `#[test]` functions with duplicate logic that should be parameterized using `#[rstest]` and `#[case]`.
- `helper_should_be_fixture`: Identifies repeated helper function calls in test setups that should be defined as `#[fixture]` functions.
- `missing_rstest`: Flags crates containing plain `#[test]` functions that lack `rstest` in `[dev-dependencies]`.
- `outdated_rstest`: Flags declared `rstest` dependencies older than `0.28.0`.

```bash
loom-gate rstest [--root .] [--format text|json|github] [--exclude GLOB]...
```

**Example:**
```bash
cargo run -p loom-gate -- rstest --root .
```

---

## Output Formats

- `text` (default): Emits formatted summaries to standard output with failing files, line numbers, and metric details, sorted by severity.
- `json`: Emits a structured JSON payload containing schema version `1`, applied thresholds, global totals, and finding arrays.
- `github`: Emits GitHub Actions workflow commands (`::error file=...,line=...::...` and `::warning file=...,line=...::...`) suitable for pull request annotations.

---

## Feature Flags

`loom-gate` is a binary crate with no optional feature flags:

| Feature | Description |
|---|---|
| `default` | Empty; builds standard binary. |

---

## Workspace & Sibling Repositories

- **`loom-rag`**: Core engine library providing the AST analysis and quality check implementations in `loom_rag::quality` (`check_loc`, `check_clones`, `check_complexity`, `check_structure`, `check_rstest`).
- **`dendrite-rag`**: Sidecar daemon exposing the identical checks via JSON-RPC 2.0 stdio methods (`quality.loc`, `quality.clones`, `quality.complexity`, `quality.structure`, `quality.rstest`).
- **`loom-eval`**: Statistical retrieval evaluation harness measuring recall, token economics, and confidence intervals across golden datasets.
- **`dendrite-core-rs`**: Sibling repository defining foundational cyclomatic and cognitive complexity algorithms (`docs/ARCHITECTURE.md` Section 2.4).
- **`dendrite-git-rs`**: Sibling repository providing Git hunk dependency graphing and review branch splitting.
- **`dendrite-plane-rs`**: Sibling repository providing Plane issue tracker integration.
- **`embedag`**: Rust library of graph building blocks (dependency ordering, Merkle delta trees, Graph-RAG primitives); `loom-rag` uses it for incremental change detection.
- **Dendrite control plane (`dendrite-control`)**: Orchestrates project directives (`dendrite directive`) and runs quality gates via `dendrite allostasis gate`.

---

## Building and Testing

Build the binary:
```bash
cargo build -p loom-gate
```

Run test suite:
```bash
cargo test -p loom-gate
```

Run CLI end-to-end integration tests:
```bash
cargo test -p loom-gate --test cli
```

---

## Specifications

- [Quality Gates Specification](../../docs/specs/quality.md): Gate algorithms, syntax rules, counting definitions, and reporting schema.
- [JSON-RPC Protocol Specification](../../docs/specs/protocol.md): JSON-RPC 2.0 stdio signatures for `quality.loc`, `quality.clones`, `quality.complexity`, `quality.structure`, and `quality.rstest`.
- [Directives Schema](../../docs/specs/directives.example.toml): Schema and default values for `.github/dendrite/directives.toml`.
