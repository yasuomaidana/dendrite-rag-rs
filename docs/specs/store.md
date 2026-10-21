# Index storage and the content-addressed segmented store

## Retrieval index layout

Default location: `<project>/.dendrite/rag/`, overridden by `Config::index_dir`.

```
.dendrite/rag/
├── ledger.redb      files, chunks, embedding cache, Merkle root, metadata
├── tantivy/         lexical index segments
└── vectors.hnsw.*   HNSW dump (only above the HNSW threshold)
```

redb tables:

| Table | Key | Value |
|---|---|---|
| `files` | path | `{blake3, size, mtime_ns, chunk_ids}` |
| `chunks` | chunk_id | chunk record (see chunking.md) |
| `embeddings` | `model_id ‖ content_hash` | `f32` vector, little-endian |
| `meta` | key | schema version, Merkle root, embed model, last indexed time |

## Incremental indexing

1. Walk the project. A file whose `(size, mtime)` matches the ledger is reused without reading its content. Every other file is read and hashed with BLAKE3.
2. Build a Merkle tree over `path → file hash` (`embedag-core` `MerkleNode::from_paths`). If its root equals the stored root, stop: nothing changed.
3. Otherwise `diff` the trees to get added, modified and removed paths. Unchanged subtrees are pruned without being visited.
4. For removed and modified files, delete their chunks from Tantivy, the vector index and the ledger.
5. Re-chunk added and modified files. Embed only chunks whose `(model, content_hash)` is not cached, then insert them everywhere.
6. Commit Tantivy and redb, and store the new Merkle root.

A rename is a delete plus an add. Its chunks keep their content hashes, so they hit the embedding cache.

## Segmented analysis store (planned, DRAG-5 / DRAG-6)

A content-addressed store for analysis artefacts (files, functions, CFG blocks, AST graphs), keyed by BLAKE3:

- Metadata index plus Zstandard-compressed blobs in `~/.dendrite/store/<project-hash>/`.
- Segment types: `file`, `function`, `cfg_block`, `ast_graph`.
- Chunk checkpoint states: `STORED` (complete), `ANALYZING_PARTIAL` (partially committed; queryable), `QUEUED_NIGHT_LOCAL` (deferred to a local run), `PAUSED_QUOTA_FLOOR` (paused at a quota reserve).
- Operations: commit a chunk atomically, list checkpoints, query committed segments (including partially analysed projects), inspect store status, and purge a project's records and unlink its blobs.
- A global CAS (`~/.dendrite/cache/cas/<blake3-root>.idx`) shares identical third-party library versions across projects, so each version is parsed and embedded once.
