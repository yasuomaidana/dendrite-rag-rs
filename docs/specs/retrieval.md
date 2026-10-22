# Retrieval

## Lexical index (Tantivy BM25)

Fields:

| Field | Tokenizer | Boost |
|---|---|---|
| `text` (header + content) | code tokenizer | 1.0 |
| `symbols` | code tokenizer | 3.0 |
| `path` | code tokenizer | 1.5 |
| `chunk_id` | raw, stored | — |

**Code tokenizer.** Each token is split on non-alphanumerics, then on case and digit boundaries (`parseHTTPResponse2` → `parse`, `http`, `response`, `2`). Everything is lowercased. The whole identifier (`parsehttpresponse2`) is also emitted, so exact identifier queries score higher than scattered sub-word matches. BM25 parameters are Tantivy's defaults (k1 = 1.2, b = 0.75).

## Dense index

`Embedder` trait: `model_id()`, `embed_documents(&[String])`, `embed_query(&str)`. Vectors are L2-normalized, so cosine similarity is a dot product.

The backend and model are the project's choice: `openai` (LiteLLM gateway or any OpenAI-compatible server), `ollama`, `hf` (Hugging Face Inference or TEI), the in-process `model2vec`, or `none`. Each has a default model. Known model families get their task prefixes and pooling automatically. [embedders.md](embedders.md) covers configuration (`.dendrite/rag.toml` and `DENDRITE_RAG_EMBED_*`), wire formats and how to choose.

**Embedding phase.** It runs after the lexical index and ledger are committed, and is resumable:

1. Drop vectors whose chunk no longer exists.
2. For every chunk without a vector, look up the cache under `(model, content_hash)`.
3. Embed only cache misses, in groups of 64 (8 under a budget), committing each group's cache entries. With `embed_budget_ms` the phase stops at the budget and reports `embed_pending`. Interactive callers therefore never wait for a first full embedding, and dense retrieval covers more chunks with each call.

If the embedder fails, the run reports `embed_error`, the lexical index stays current, and the next run picks up the missing vectors. Changing the model invalidates the vector index but not other models' caches.

**Vector index: exact int8 scan.**
- Each vector is stored as `i8` components plus one `f32` scale (`max|x| / 127`). The query is quantized the same way, and the score is an integer dot product times both scales. That is 4× less memory than `f32` (50k chunks × 768 dims ≈ 38 MB instead of 150 MB), and the loop auto-vectorizes.
- Rows are scanned in parallel blocks of 1,024, and the top k comes from `select_nth_unstable`.
- The int8 scan shortlists `4 × top_k` rows (at least 40), which are rescored with their exact `f32` vectors from the embedding cache. Recall@10 against exact search: 0.967 at 49k chunks / 256 dims (0.908 without rescoring), 1.000 on the Dendrite repository / 768 dims. Results in `eval/RESULTS.md`.
- The index persists to `vectors.bin` (written atomically) and loads at open.

**Measured on the Dendrite repository** (5,281 chunks, `nomic-embed-text` through the LiteLLM gateway on Ollama, Apple Silicon):
- First embedding: 558 s (about 9.5 chunks/s; Ollama serializes requests, and 4 concurrent requests gained only about 10%).
- Re-index with nothing changed: 40 ms.
- Dense query: 28–35 ms, almost all of it embedding the query; the scan itself is sub-millisecond.

The cache makes the first embedding a one-time cost per content hash.

Query vectors are cached too: an LRU of 256 entries (`[index] query_cache`, `DENDRITE_RAG_QUERY_CACHE`; 0 disables it) keyed by `(model, query text)`, so a repeated query skips the embedder, which is almost all of dense query latency.

**Approximate index: HNSW (`feature = "hnsw"`, DRAG-37).**
- `HnswIndex` (M = 16, 32 on layer 0, ef_construction = 200) sits behind the `VectorIndex` trait and wraps the `FlatIndex`: distances are computed over the same int8 components and scales, so no second `f32` copy is kept; the graph (neighbour lists, about 150–250 bytes per vector, estimated) is the only addition.
- `[index] vector_index = "auto" | "flat" | "hnsw"` (`DENDRITE_RAG_VECTOR_INDEX`); `auto` switches to HNSW above 100,000 vectors when the feature is compiled, else stays flat. `[index] hnsw_ef_search` (`DENDRITE_RAG_HNSW_EF_SEARCH`, default 128) trades recall for speed.
- The graph persists to `vectors.hnsw` ([store.md](store.md)); a missing or corrupt file is rebuilt from `vectors.bin`. Deleted vectors are tombstones until they exceed 20% of the nodes, then the graph is rebuilt.
- Measured (`recall_report`, 20,000 unit vectors of 64 dimensions, 200 queries, Apple Silicon, release build): recall@10 of the engine's 40-candidate shortlist rescored with exact vectors is 0.824 / **0.954** / 0.990 at ef_search 64 / **128** / 200 on uniform random vectors, and 0.908 / **0.974** / 0.991 on clustered ones, which resemble real embeddings more. At this size the parallel flat scan is faster (62 µs per query against 145–210 µs at ef_search 128); the flat scan grows linearly, so the break-even is estimated near 100,000 vectors, which is why `auto` switches there. That threshold is an estimate, not yet measured on a corpus of that size.

## Symbol-name ranking

Query tokens that look like identifiers (contain `_`, `::`, `.` or an inner capital, or match a known symbol) are looked up in the symbol table. Chunks that define the symbol rank first, then chunks that reference it.

## Fusion

Weighted Reciprocal Rank Fusion over the ranked lists:

`score(d) = Σ_i w_i / (k + rank_i(d))`, with k = 60 and default weights lexical 1.0, dense 1.0, symbol 1.5.

RRF uses ranks only, so BM25 and cosine scores never need calibrating against each other. Each list contributes its top `candidates` (default 50).

## Optional rerank

A `Reranker` trait scores `(query, chunk)` pairs with a cross-encoder. It is applied to the top 30 fused candidates. Off by default.

Backends (`[rerank] backend`):
- `gateway` (default): server with a Cohere-style `/v1/rerank` endpoint (e.g. LiteLLM gateway, model `bge-reranker-v2-m3`). Off when `model` is unset.
- `candle`: in-process cross-encoder on candle (feature `candle`, default model `BAAI/bge-reranker-base`). Checkpoints with `model_type` `xlm-roberta` (the BGE rerankers) run through candle's XLM-RoBERTa sequence classifier, and `bert` ones (MiniLM-style cross-encoders) through BERT with its pooler and linear head. Runs on the GPU (Metal, CUDA) or CPU. Its logits match the reference implementation (transformers) within 0.05 on `BAAI/bge-reranker-base` (`live_candle_bge_reranker_scores`).
- `none`: reranking disabled.

## Context packing

`context(query, token_budget)`:

1. Take the fused hits.
2. **Expand**: for each top hit, add chunks linked in the symbol graph (definitions of the symbols it references, callers of the symbols it defines), each scored with the parent's score × 0.5 per hop (max 1 hop).
3. **Rank by importance**: a personalized PageRank over the file-level reference graph, seeded with the hits' files, breaks ties in favour of central code (the repository-map technique).
4. **Remove overlap**: drop chunks fully contained in a higher-ranked chunk's line span, and merge adjacent spans in the same file.
5. **Pack** greedily by score until the token budget is reached. Tokens are estimated at ~4 characters per token with a 1.15 safety factor; callers that need exact counts pass a smaller budget.

The result lists the spans in score order, each with `path`, `start_line`, `end_line`, `scope`, `score` and `content`, plus the total estimated tokens.

## Performance targets

| Metric | Target |
|---|---|
| RSS at 50k chunks | < 200 MB (measured 96 MB lexical baseline at 49k; ~135 MB with 768-d vectors) |
| p95 search latency, 8 concurrent readers | < 30 ms (embedding the query excluded) |
| Quantized recall@10 vs exact | ≥ 0.95 |
| Hybrid recall@10 on the golden set | ≥ lexical-only |
