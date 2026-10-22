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
