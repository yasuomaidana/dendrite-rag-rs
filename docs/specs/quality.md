# Quality gates (`loom-gate`, `quality.loc`)

`loom-gate` checks a source tree against the quality directives of a project. It has three gates: production lines per file (`loc`), repeated code (`clones`) and Rust test structure (`rstest`).

    loom-gate loc [--root .] [--config <directives.toml>] [--format text|json|github] [--max N] [--warn-at N] [--exclude GLOB]…

| Exit | Meaning |
|---|---|
| 0 | the gate passed (files over `warn_at` only warn) |
| 1 | a file has more production lines than `max_production_lines` |
| 2 | the tool could not run: bad flags, an unreadable or invalid directives file, a limit that contradicts another |

Each gate is also a JSON-RPC method (`quality.loc`, `quality.clones`, `quality.rstest`) of `dendrite-rag serve` ([protocol.md](protocol.md)), which `dendrite allostasis gate` calls.

## What counts

A file's **production lines** are its lines that hold at least one non-blank character outside comments and outside test code. A line with code and a trailing comment is a code line.

Found with tree-sitter, so they are exact, not pattern guesses:

| Language | Not production code |
|---|---|
| all with a grammar | every comment, including doc comments |
| Rust | an item preceded by `#[cfg(test)]` (also `cfg(any(test, ...))`; `cfg(not(test))` stays production), `#[test]`, `#[rstest]`, `#[tokio::test]`, `#[test_case]`, `#[wasm_bindgen_test]`, `#[...::test]`; a file whose first attribute is `#![cfg(test)]` |
| Python | docstrings; `test_*` functions and `Test*` classes (decorators included); the `if __name__ == "__main__":` block |
| Java | methods annotated `@Test`, `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`; classes named `*Test` or `*Tests` |
| JavaScript, TypeScript | top-level `describe`, `it`, `test`, `beforeEach`, `afterEach`, `beforeAll`, `afterAll` calls |
| Go, C, C++ | none inside a file; tests are whole files |

Whole files are test files by name or place: a `tests/`, `test/` or `__tests__/` directory; `*_test.*`; `*.test.*`; `*.spec.*`; `test_*.py`; `conftest.py`; `*Test.java`; `test_*.c`, `.cc`, `.cpp`. Their non-blank lines count as test lines.

Code files without a grammar here (`.sh`, `.bash`, `.kt`, `.kts`, `.swift`, `.rb`, `.php`, `.cs`, `.scala`, `.lua`, `.zig`) count every non-blank line as production. Kotlin `@Test` stripping waits for a grammar. Markdown, JSON, YAML and the like are not code and are not counted.

The walk honours `.gitignore` and always skips `.git`, `target`, `node_modules`, `vendor`, `dist`, `.loom`, `__pycache__` and `.venv`. A file the grammar cannot fully parse is counted anyway and reported with `parse_errors: true`.

## Limits

`[loc]` of `.github/dendrite/directives.toml` (written by `dendrite directive`; [directives.example.toml](directives.example.toml) is the shape both sides parse in their tests):

| Key | Default | Meaning |
|---|---|---|
| `max_production_lines` | 500 | more than this fails the gate |
| `warn_at` | 450 | more than this (and not more than the maximum) warns |
| `exclude` | `[]` | globs, relative to the root, of files not counted |

A missing file or table means the defaults. `max_production_lines` must be above 0 and `warn_at` must not exceed it. Flags and the RPC params override the file.

## Report

`--format json` (and the `quality.loc` result) is stable; `schema` is bumped when it changes:

```json
{
  "schema": 1,
  "max_production_lines": 500,
  "warn_at": 450,
  "totals": {"files": 137, "production": 17354, "test": 5904, "warnings": 0, "failures": 1},
  "files": [
    {"path": "src/lib.rs", "language": "rust", "production": 501, "test": 300, "comment": 12, "blank": 40,
     "status": "fail", "parse_errors": false}
  ]
}
```

`--format github` prints one workflow command per file that warns or fails, for annotations in a pull request:
`::error file=src/lib.rs,line=1,title=Production lines::501 production lines, over 500`. `--format text` lists the files that warn or fail, worst first, and a summary line.

## Duplication (`loom-gate clones`)

    loom-gate clones [--root .] [--config <directives.toml>] [--format text|json|github] [--max-percent P] [--min-lines N] [--min-tokens N] [--include-tests true|false] [--exclude GLOB]…

Finds blocks of code that repeat: two syntax subtrees are the same when they match after every identifier and every literal is replaced by a wildcard, so renamed copy-paste and copies with other constants are found, and comments and layout do not matter. A block counts when it has at least `min_lines` lines and `min_tokens` leaves of the syntax tree. Only the largest repeated block is reported: a repeated function is not also reported as the loop and the `if` inside it. It works in every language with a grammar (Go, Rust, Python, TypeScript, TSX, JavaScript, Java, C, C++).

The check passes when the share of code lines that sit in a repeated block is at most `max_percent` (0 by default: no duplication). Exit codes are those of `loc`.

`[duplication]` of the directives file:

| Key | Default | Meaning |
|---|---|---|
| `max_percent` | 0.0 | share of the counted lines that may be duplicated, 0 to 100 |
| `min_tokens` | 50 | leaves of the syntax tree a repeated block needs, at least 10 |
| `min_lines` | 5 | lines a repeated block needs, at least 2 |
| `include_tests` | true | whether test code (files and inline test modules, see above) is checked too |

The JSON report has `schema`, the limits used, `totals` (`files`, `lines`, `duplicated_lines`, `percent`, `groups`) and `groups`, each with its `tokens` and every `location` (`path`, `start_line`, `end_line`). `--format github` prints an annotation on every location, an error when the check fails and a warning when the duplication is within the allowance.

## Rust test structure (`loom-gate rstest`)

    loom-gate rstest [--root .] [--format text|json|github] [--exclude GLOB]…

Fails when plain `#[test]` functions of the same module:

- **`repeated_test`**: two or more have bodies that are the same up to literals (and are at least 3 lines): they are one `#[rstest]` with a `#[case]` each;
- **`helper_should_be_fixture`**: two or more bind the result of the same undecorated helper (`let store = open();`): the helper is a `#[fixture]` that `#[rstest]` tests take as an argument.

Tests that are already `#[rstest]`, `#[case]` or `#[fixture]` are not looked at. Exit 1 when there is a violation.
