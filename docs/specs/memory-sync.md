# Memory synchronization modes (planned, DRAG-2)

Two storage tiers:

- **Tier 1 (local):** the project index and segmented store on the developer machine.
- **Tier 2 (shared):** PostgreSQL 16 with `pgvector`, run by the shared Dendrite server.

Records are keyed by commit SHA plus chunk content hash, so every write is an idempotent upsert.

| Mode | Behaviour |
|---|---|
| `local-only` | Resolve the active multi-repository dependency slice. Fetch only the chunks, graphs and library slices for those repositories, then serve queries with networking disabled. |
| `edge` | Read-through cache over Tier 2. Writes go to Tier 2 when it is reachable; otherwise they append to `.dendrite/memory-wal.journal`. A probe every 30 s replays the journal with idempotent upserts. |
| `push` | Upload local deltas (chunks, embeddings, session summaries), then compact Tier 2: delete chunks whose commit is unreachable from every tracked branch, delete orphaned blobs, and deduplicate vectors by content hash and by cosine > 0.995 within the same source. Runs in batches, supports `--dry-run`, and prints a report. |
