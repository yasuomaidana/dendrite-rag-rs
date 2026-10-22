# dendrite-rag-rs

Retrieval engine for code and documentation: cAST syntax-tree chunking, code-aware BM25 (Tantivy), dense embeddings with flat/HNSW vector search, weighted Reciprocal Rank Fusion, separate code and documentation pipelines, symbol-graph expansion and token-budgeted context packs with `path:line` citations, down to outline maps. Indexes are incremental (BLAKE3 + Merkle diff) and stored locally in the project.

The workspace holds `loom-rag` (the engine library, no Dendrite references), `dendrite-rag` (the binary that speaks JSON-RPC 2.0 over stdio for `dendrite-control` and `dendrite-mcp-hub`, with Dendrite's settings and `ask`), and `loom-eval` (quality, token and latency evaluation against baselines: [eval/RESULTS.md](eval/RESULTS.md)).

- [Architecture](docs/ARCHITECTURE.md)
- [Specifications](docs/README.md)
- [Responsibilities and ticket map](docs/RESPONSIBILITIES.md)
