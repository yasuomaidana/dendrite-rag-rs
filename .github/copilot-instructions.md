# GitHub Copilot Custom Instructions

> Authoritative developer guidelines and architectural constraints for AI code generation.

## 1. Zero Code Without Plane Ticket Mandate
- No agent, AI assistant, or engineer is permitted to author code or modify tests without an active, authoritative ticket in Plane v2 (`http://localhost:8090`).
- Every commit must reference an active Plane ticket (e.g., `[DEN-XXX]`).

## 2. Test-Driven Development (TDD) & Anti-Mock Mandate
- Author tests before or alongside code implementation.
- **Zero Mock Policy**: Do NOT use mocking frameworks, fake stubs, or synthetic dummies for persistence or protocols.
- All persistent database layers (Postgres, Redis, MinIO) must use **Testcontainers** for real integration tests.
- Maintain minimum test coverage: **>= 85%** (`coverage.min_percent` in `.github/dendrite/directives.toml`).

## 3. Approved & Enforced Toolchains
This project uses these toolchains; all tooling and dependency operations go through them:
- **Rust**: `cargo` (`cargo test`, `cargo clippy`, `cargo fmt --check`)

## 4. Technology Stack & Coding Standards
- **Primary Stack**: RUST
- **Active Toolchains**: cargo
- **Enforced Linters**: clippy
- **Complexity Limits**: cyclomatic complexity <= 14 and cognitive complexity <= 20 per function, at most 500 production lines per file (`.github/dendrite/directives.toml`).

## 5. Model Precedence & Zero-Cost Delegation
- Commit messages and repetitive test digestions must be delegated to local zero-cost models (`qwen3.8:27b` / Ollama Metal) to conserve API tokens.
- Cloud models (Gemini Pro, Copilot Sonnet) are reserved for macro architecture, complex decomposition, and review gates.
