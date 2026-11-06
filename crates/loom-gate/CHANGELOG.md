# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-02

### Added
- **Command-Line Interface (`loom-gate`)**:
  - Quality gates CLI tool evaluating source code trees against project directives configured in `.github/dendrite/directives.toml` (`[DRAG-63]`).
  - Standardized exit codes: `0` (pass, warnings allowed), `1` (gate limit breached), and `2` (tool invocation or configuration error).
  - Multiple output formats: human-readable text (`--format text`), structured JSON (`--format json`, schema version 1), and GitHub Actions workflow command annotations (`--format github`).
- **Production Lines of Code Gate (`loc`)**:
  - Exact Tree-sitter AST line counting stripping comments, doc comments, and inline test modules (`#[cfg(test)]`) across supported languages.
  - Configurable limits via `[loc]` table (`max_production_lines` default 500, `warn_at` default 450, and `exclude` glob patterns).
- **Code Duplication Gate (`clones`)**:
  - Syntax subtree clone detection with normalized identifiers and literals, reporting largest repeated blocks.
  - Configurable limits via `[duplication]` table (`max_percent` default 0.0, `min_tokens` default 50, `min_lines` default 5, and `include_tests` default true).
- **Function Complexity Gate (`complexity`)**:
  - Function-level McCabe cyclomatic complexity (default max 10) and SonarSource cognitive complexity (default max 15) gates across 8 programming languages.
- **Directory Structure Gate (`structure`)**:
  - Modularity enforcement rules: maximum modules per directory (`max_files_per_dir` default 12), maximum cumulative lines per directory (`max_dir_production_lines` default 3000), and maximum common prefix grouping (`max_prefix_group` default 4).
- **Rust Test Organization Gate (`rstest`)**:
  - Verifies idiomatic test structure by detecting duplicate plain `#[test]` functions that should use `#[rstest]` with `#[case]`, setup helpers that should be `#[fixture]`, and missing or outdated `rstest` dependencies (`< 0.28.0`).
- **Integration Tests**:
  - End-to-end integration test suite (`tests/cli.rs`) verifying exit codes, limits, exclusion filters, and output formatting.
