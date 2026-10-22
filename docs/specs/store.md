# Index storage and the content-addressed segmented store

## Retrieval index layout

Default location: `<project>/.dendrite/rag/`, overridden by `Config::index_dir`.

```
.dendrite/rag/
├── ledger.redb      files, chunks, embedding cache, Merkle root, metadata
├── tantivy/         lexical index segments
├── vectors.bin      int8 vector index (ids, scales, components)
└── vectors.hnsw     HNSW graph index (when hnsw feature / index is enabled)
```

redb tables:

| Table | Key | Value |
|---|---|---|
| `files` | path | `{blake3, size, mtime_ns, chunk_ids, imports}` |
| `chunks` | chunk_id | chunk record (see chunking.md) |
| `embeddings` | `model_id ‖ 0x00 ‖ content_hash` | `f32` vector, little-endian (the cache; kept across rebuilds) |
| `meta` | key | schema version, Merkle root, embed model, last indexed time |
| `community_files`, `community_chunks`, `communities` | path / chunk id / id | community of each file and chunk, and each community's record ([graph.md](graph.md)) |

The schema version is `2` (version 2 added `calls`, `method_calls` and `imports`). A ledger with another version is cleared on open, keeping the embedding cache, so the next `index` re-chunks every file without re-embedding unchanged content.

`vectors.bin` (format `DRV3`): magic, `u32` dimension, `u32` row count, then per row a `u16`-length id, a `u16`-length tag, an `f32` scale and the `i8` components, followed by a BLAKE3 checksum of everything before it. It is written to a temporary file that is fsynced before the rename, and the directory is fsynced after. `DRV2` files (with `u8` lengths and no checksum) still load. A missing, truncated or corrupt file is not an error: the engine starts with an empty vector index and the next `index` refills it from the `embeddings` cache, without calling the embedder. That run also compares the stored ids with the ledger's, so a stale file with the right row count is repaired too.

`vectors.hnsw` (format `DRH1`): magic, `u32` version (`1`), `u32` M, `u32` ef_construction, `i64` entry_point, `u32` max_level, `u32` nodes_count, followed by per-node records (`u16` id length, id bytes, `u32` level, `u8` deleted flag, tombstone vector when deleted, and `u32` neighbor lists per level) and a 32-byte BLAKE3 checksum trailer. Written atomically via a temporary file fsynced before rename. A missing or corrupt `vectors.hnsw` is rebuilt from `vectors.bin` on load, never an error. Graph rebuilds occur automatically when tombstones exceed 20% of nodes.

## Storage backends

Engine access to index storage is abstracted behind traits in `src/store.rs`:

- `LedgerStore` / `LedgerWrite`: file metadata, chunk records, embedding cache, metadata entries, and graph communities.
- `LexicalStore` / `LexicalWrite`: full-text lexical search and chunk document indexing.
- `VectorIndex`: nearest-neighbour vector search over quantized embeddings.

The `Store` struct aggregates these handles (`ledger`, `lexical`, `vectors`):

- **LocalStore** (current implementation, via `Store::open_local`):
  - Ledger: redb (`ledger.redb`).
  - Lexical: Tantivy BM25 index (`tantivy/`).
  - Vectors: binary FlatIndex (`vectors.bin`) and optional in-memory HNSW graph (`vectors.hnsw`, feature `hnsw`) with self-healing on corruption.
- **PgStore** (planned in DRAG-33): PostgreSQL backend storing ledger tables, BM25 / tsvector lexical search, and vector embeddings directly in PostgreSQL.

## Incremental indexing

1. Walk the project. A file whose `(size, mtime)` matches the ledger reuses its stored hash without being read. Every other file is read and hashed with BLAKE3 (in parallel).
2. Build Merkle trees over `path → file hash` for the ledger and for the walk (`embedag::dag::merkle::MerkleNode::from_paths`) and `diff` them. Equal subtrees are pruned, so the cost follows the size of the change. A path in the delta that names a directory stands for every file under it.
3. A file whose content is unchanged but whose mtime moved only gets its ledger entry refreshed.
4. For removed and modified files, delete their chunks from Tantivy, the vector index and the ledger.
5. Re-chunk added and modified files. Embed only chunks whose `(model, content_hash)` is not cached, then insert them everywhere.
6. Commit Tantivy first, then redb with the new Merkle root and `last_indexed`. Re-running after a crash between the two is safe: files are removed from Tantivy by path before their chunks are added again.

Measured on the Dendrite repository (828 files, 5,281 chunks, Apple Silicon, lexical only): full index 0.93 s, no-op re-index 43 ms, BM25 query 0.1–0.5 ms.

A rename is a delete plus an add. Its chunks keep their content hashes, so they hit the embedding cache.

## Segmented analysis store (planned, DRAG-5 / DRAG-6)

A content-addressed store for analysis artefacts (files, functions, CFG blocks, AST graphs), keyed by BLAKE3:

- Metadata index plus Zstandard-compressed blobs in `~/.dendrite/store/<project-hash>/`.
- Segment types: `file`, `function`, `cfg_block`, `ast_graph`.
- Chunk checkpoint states: `STORED` (complete), `ANALYZING_PARTIAL` (partially committed; queryable), `QUEUED_NIGHT_LOCAL` (deferred to a local run), `PAUSED_QUOTA_FLOOR` (paused at a quota reserve).
- Operations: commit a chunk atomically, list checkpoints, query committed segments (including partially analysed projects), inspect store status, and purge a project's records and unlink its blobs.
- A global CAS (`~/.dendrite/cache/cas/<blake3-root>.idx`) shares identical third-party library versions across projects, so each version is parsed and embedded once.

## PostgreSQL (shared tier, DRAG-33)

`[storage] mode = "remote"` (or `DENDRITE_RAG_STORAGE_MODE=remote`) keeps the whole index in PostgreSQL 16 with pgvector, built with the `postgres` feature. The URL comes only from `DENDRITE_RAG_PG_URL` (Dendrite writes it to `$DENDRITE_HOME/rag.env`, DEN-1047), never from the settings file. A repository is named by `[storage] repo`, else its `origin` URL, else its path.

- **One database for a team.** Every table is keyed by repository, except `rag_embeddings`, keyed by `(model, content_hash)` only: an embedding computed on one machine serves every repository and every machine. A second repository with the same content embeds nothing.
- **Tables:** `rag_repos`, `rag_files`, `rag_chunks`, `rag_meta`, `rag_embeddings`, `rag_vectors` (pgvector), `rag_community_files`, `rag_community_chunks`, `rag_communities`, and for BM25 `rag_docs` (field lengths) and `rag_postings` (`term, field, tf`). The schema is created on open (`rag_schema` holds its version; a newer one is refused).
- **Writes** are buffered and applied in one transaction per commit, as upserts and keyed deletes, so an interrupted run replays safely, like the local store.
- **BM25 in SQL** (`src/store/pg.rs`): postings come from `code_terms`, the same tokenizer as Tantivy, and the query applies Okapi BM25 (k1 1.2, b 0.75) with the same field boosts. Lexical MRR@10 on the golden sets: 0.631 against Tantivy's 0.614 (Dendrite) and 0.864 against 0.844 (embedag); Tantivy quantizes field lengths, the SQL does not. A commit adding more than 500 documents runs `ANALYZE`, which the query plan depends on.
- **Vectors** are stored in `rag_vectors` and mirrored in memory at open for the scan (repository-sized); changes reach Postgres on `persist`.
- **Measured** (Dendrite, 833 files, 5,286 chunks, local Docker, lexical only): first index 15 s, communities 0.35 s, search with 8 concurrent readers p50 36 ms / p95 104 ms over a pool of 4 connections (a single query takes about 6 ms in the database). Local storage stays the default for latency (p50 0.4 ms).
- `purge` removes only the local index directory; repository rows stay in the shared tier.

## Edge mode (DRAG-34)

`[storage] mode = "edge"` keeps the local store (redb, Tantivy, flat or HNSW vectors) for every read and write, so queries keep local latency and work offline, and mirrors every commit to the shared tier (`src/store/edge.rs`):

- **Mirroring.** Each ledger, lexical and vector commit is applied to PostgreSQL right after the local commit, as the same upserts remote mode uses.
- **Write journal.** When the tier is unreachable the commit is appended to `memory-wal.journal` in the index directory (one JSON batch per line, fsynced; inside the index directory so it stays out of version control). Before the next mirror, at open, and on `sync`, the journal is replayed in order and emptied; replaying twice changes nothing, since every statement is an upsert or a keyed delete. A line cut by a crash mid-write is skipped. While the tier is down it is tried again at most every 30 s, so an outage costs one connection attempt per interval, not one per commit.
- **Read-through embeddings.** An embedding missing from the local cache is looked up in the shared tier and cached locally before anything is embedded, so a fresh checkout or a new machine reuses what the team has already computed (the test `reads_embeddings_through_from_the_shared_tier` indexes a project with zero embedder calls).
- **`pull`** (RPC and CLI) fetches every embedding the current chunks need and fills the vector index, to prepare for working offline. Chunking and the lexical index are rebuilt locally (about 1.6 s for the Dendrite repository), which is why edge mode shares embeddings, the expensive part (about 17 minutes for the same repository on candle Metal), rather than index segments.
- `status` reports `sync_pending`, the commits still in the journal.

The spec's Merkle-subtree pull was not built: measured, re-chunking is three orders of magnitude cheaper than embedding, and read-through gives the saving with no extra protocol. At 49k chunks a cold local rebuild takes 14 s (DRAG-42, eval/RESULTS.md); segment pull is worth building only for corpora about ten times larger. Fetching the slice of a synapse (several repositories) is a loop over `pull` per repository, for Dendrite to drive.

## Push and compaction (DRAG-35)

`dendrite-rag push <root> [--dry-run]` (RPC `push`, feature `postgres`) works from any storage mode and needs `DENDRITE_RAG_PG_URL`:

1. **Upload.** The repository's current state in the shared tier is replaced by the local index: files, chunks, lexical postings, vectors, communities and metadata. Embeddings the tier already holds are not sent again (`embeddings_uploaded` counts the new ones).
2. **Record.** HEAD and up to 5,000 ancestors go to `rag_commits` (with their parents), the clone's local and `origin/*` branches become the tracked heads in `rag_branch_heads` (a branch deleted everywhere stops being tracked), and `rag_snapshots` records which chunks (`path`, content hash) the HEAD commit holds.
3. **Compact.** Snapshots and commits that no tracked head reaches (a recursive walk over the parents) are deleted; embeddings used only by snapshots that are near-duplicates (pgvector cosine distance < 0.005, that is cosine > 0.995) of the current version of the same path are deleted; embeddings no current chunk of any repository and no snapshot uses are deleted (content-hash keys already deduplicate identical content); lexical, vector and community rows whose chunk is gone are deleted.

All of it runs in one transaction: a failure leaves the tier as it was, and `--dry-run` runs the same statements and rolls them back, so its report is exactly what the real push does. The trade-off is one long transaction instead of batches; at team scale that was measured acceptable: a push of the Dendrite repository (833 files, 5,286 chunks, lexical only) takes 18 s, 13 s of it the upload. Snapshots are not used by retrieval yet; they bound what compaction keeps and are the basis for history-aware search.
