# dendrite-rag-rs Documentation

- [Architecture](ARCHITECTURE.md): the retrieval pipeline, module layout and design principles.
- [Responsibilities](RESPONSIBILITIES.md): ownership boundary with the sibling repositories, interfaces, and the Plane ticket map.

## Specifications

- [Walking and chunking](specs/chunking.md): file discovery, cAST syntax-tree chunking, chunk records and contextual headers.
- [Retrieval](specs/retrieval.md): code-aware BM25, embeddings and vector indexes, symbol ranking, weighted RRF, reranking, context packing and performance targets.
- [Storage](specs/store.md): index layout, the redb ledger, Merkle-based incremental indexing, and the planned segmented analysis store.
- [Protocol](specs/protocol.md): the `dendrite-rag` JSON-RPC 2.0 stdio methods and error codes.
- [Memory synchronization](specs/memory-sync.md): planned `local-only`, `edge` and `push` modes.
- [Topic memory](specs/topic-memory.md): planned structured topic memory parser and fidelity guard.
