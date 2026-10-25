# Memory synchronization modes

Implemented: `local` (the local index: redb ledger with SQLite opt-in via feature `sqlite`, DRAG-49), `remote` ([store.md](store.md) § PostgreSQL, DRAG-33), `edge` ([store.md](store.md) § Edge mode, DRAG-34, with read-through embeddings instead of a slice download) and `push` with compaction ([store.md](store.md) § Push and compaction, DRAG-35). The multi-repository slice mode `local-only` (DRAG-2) is still planned.

Two storage tiers:

- **Tier 1 (local):** the local project index (redb or SQLite ledger, Tantivy lexical index, and vector index in `vectors.bin` / `vectors.hnsw`) on the developer machine. (The segmented analysis store, DRAG-5/6, is planned.)
- **Tier 2 (shared):** PostgreSQL 16 with `pgvector`, run by the shared Dendrite server.

In PostgreSQL, tables are keyed by repository and item id or path; embeddings are keyed by model and content hash; commits by SHA; snapshots record which chunks (`path`, content hash) each commit holds. Writes are buffered and applied in transactions as idempotent upserts or keyed deletes.

| Mode | Behaviour |
|---|---|
| `local` | Local indexing and search on the developer machine (redb ledger or SQLite opt-in, Tantivy BM25, exact int8 or HNSW vectors). Networking is not used. |
| `local-only` (planned, DRAG-2) | Resolve the active multi-repository dependency slice. Fetch only the chunks, graphs and library slices for those repositories, then serve queries with networking disabled. |
| `edge` | Local store serving every read and write, with commits mirrored to Tier 2. Writes go to Tier 2 when it is reachable; otherwise they append to `memory-wal.journal` in the index directory. Replayed at open, on `sync`, and before each mirror; unreachable Tier 2 is retried at most every 30 s. Missing embeddings are read through from Tier 2. |
| `push` | Upload the local index (files, chunks, postings, vectors, communities, metadata; existing embeddings skipped), record the commit graph and snapshot, then compact Tier 2: delete snapshots and commits unreachable from every tracked branch head, delete embeddings near-duplicate (cosine distance < 0.005 / cosine > 0.995) to a newer version of the same path used only by snapshots, delete orphaned embeddings no chunk or snapshot uses, and delete orphaned lexical, vector and community rows. Runs in one transaction, supports `--dry-run`, and prints a report. |
