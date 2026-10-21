# dendrite-rag-rs

Retrieval engine for source-code repositories: cAST syntax-tree chunking, code-aware BM25 (Tantivy), dense embeddings with flat/HNSW vector search, weighted Reciprocal Rank Fusion, symbol-graph expansion and token-budgeted context packs with `path:line` citations. Indexes are incremental (BLAKE3 + Merkle diff) and stored locally in the project.

It ships the `dendrite-rag` binary, which speaks JSON-RPC 2.0 over stdio and is used by `dendrite-control` and `dendrite-mcp-hub`, and the `dendrite_rag` library.

- [Architecture](docs/ARCHITECTURE.md)
- [Specifications](docs/README.md)
- [Responsibilities and ticket map](docs/RESPONSIBILITIES.md)
