# dendrite-rag-rs

Rust BM25+vector hybrid retrieval, content-addressable store and local-only/edge/push memory sync engine (Pillar 7/9)

`dendrite-rag-rs` owns Dendrite's hybrid Okapi BM25 + dense vector retrieval engine (with Reciprocal Rank Fusion), the BLAKE3 content-addressable persistent segmented store (CAS) with chunk checkpointing and purge lifecycle, structured topic memory parsing with zero-extrapolation fidelity guards, and the multi-tier memory synchronization engine (`local-only`, `edge` WAL, and `push` compaction).
It is designed to ship a `dendrite-rag` sidecar binary speaking JSON-RPC 2.0 over stdio (`proto/*.json`) consumed by `dendrite-control` and `dendrite-mcp-hub`, while leaving CLI/UI orchestration, token governance, AST/CFG analysis (`dendrite-core-rs`), and Git hunk clustering (`dendrite-git-rs`) to sibling repositories.
See [docs/RESPONSIBILITIES.md](docs/RESPONSIBILITIES.md) for the complete boundary definition, extraction map, and ticket tracking.
