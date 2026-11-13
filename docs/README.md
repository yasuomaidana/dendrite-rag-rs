# dendrite-rag-rs Documentation

- [Architecture](ARCHITECTURE.md): the retrieval pipeline, module layout, and design principles.
- [Responsibilities](RESPONSIBILITIES.md): ownership boundaries with sibling repositories, interfaces, and the Plane ticket map.

## Specifications

- [Walking and chunking](specs/chunking.md): file discovery, cAST syntax-tree chunking, chunk records, and contextual headers.
- [Retrieval](specs/retrieval.md): code-aware BM25, embeddings and vector indexes, symbol ranking, weighted RRF, reranking, context packing, and performance targets.
- [Embedders](specs/embedders.md): embedding backends and models per project (`.dendrite/rag.toml`, `DENDRITE_RAG_*`), wire formats, and model conventions.
- [Storage](specs/store.md): index layout, redb and SQLite ledgers, Merkle-based incremental indexing, PostgreSQL shared tier, and the planned segmented analysis store.
- [Code graph and communities](specs/graph.md): typed code graph, Leiden community detection, god nodes, graph queries, and optional LLM summaries.
- [Git history search](specs/history.md): commit message and per-file diff documents, incremental indexing, and history search.
- [Protocol](specs/protocol.md): JSON-RPC 2.0 stdio methods, request and response parameters, CLI subcommands, and error codes.
- [Memory synchronization](specs/memory-sync.md): planned `local-only`, `edge`, and `push` synchronization modes.
- [Topic memory](specs/topic-memory.md): planned structured topic memory parser and zero-extrapolation fidelity guard.
- [Ask](specs/ask.md): retrieval-augmented generation workflow, prompt construction, local/hybrid/paid model tiers, and answer caching.
- [Dendrite integration](specs/dendrite-integration.md): how Dendrite runs the sidecar (one process per call, major-version handshake), what a client does with each result field, and the open items on both sides.

## Crates and evaluation

- [`loom-rag`](../crates/loom-rag/README.md): the retrieval engine library, its cargo features and modules.
- [`dendrite-rag`](../crates/dendrite-rag/README.md): the sidecar, the CLI, settings and `ask`.
- [`loom-eval`](../crates/loom-eval/README.md): the evaluation harness (golden sets, paired confidence intervals, SWE-bench, abstention calibration).
- [Evaluation results](../eval/RESULTS.md): every measurement behind a default, with the decisions it led to.
