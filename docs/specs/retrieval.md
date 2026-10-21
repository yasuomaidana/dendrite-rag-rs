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

`Embedder` trait: `embed(&[String]) -> Vec<Vec<f32>>`, plus `dim()` and `model_id()`.

| Backend | When | Notes |
|---|---|---|
| `GatewayEmbedder` | default | OpenAI-compatible `POST {base}/v1/embeddings`, batches of 64, bearer key from `LITELLM_API_KEY`/`OPENAI_API_KEY`, base from `LITELLM_PROXY_URL` (default `http://localhost:4000`). Model from `Config::embed_model` (default `nomic-embed-text`) |
| `Model2VecEmbedder` | feature `model2vec`, or `embed_backend = "model2vec"` | In-process static embeddings (model2vec-rs), no network, sub-millisecond per chunk; lower quality than a transformer, but a full offline fallback |
| none | `embed_backend = "none"` | Lexical and symbol retrieval only |

Vectors are L2-normalized, so cosine similarity is a dot product. Embeddings are cached in the ledger under `(model_id, content_hash)`. Changing models invalidates only that model's cache.

`VectorIndex` trait: `upsert`, `remove`, `search(query, k)`, `len`.

- **Flat** (default below `hnsw_threshold`, 20,000 vectors): exact dot-product scan. At this size an exact scan is faster than a graph search and has perfect recall.
- **HNSW** (`hnsw_rs`, cosine, M = 16, ef_construction = 200, ef_search = 64): used above the threshold. Deletions are tombstoned, and the graph is rebuilt when tombstones exceed 20%.

## Symbol-name ranking

Query tokens that look like identifiers (contain `_`, `::`, `.` or an inner capital, or match a known symbol) are looked up in the symbol table. Chunks that define the symbol rank first, then chunks that reference it.

## Fusion

Weighted Reciprocal Rank Fusion over the ranked lists:

`score(d) = Σ_i w_i / (k + rank_i(d))`, with k = 60 and default weights lexical 1.0, dense 1.0, symbol 1.5.

RRF uses ranks only, so BM25 and cosine scores never need calibrating against each other. Each list contributes its top `candidates` (default 50).

## Optional rerank

A `Reranker` trait scores `(query, chunk)` pairs with a cross-encoder through the gateway's `/v1/rerank` endpoint (for example `bge-reranker-v2-m3`). It is applied to the top 30 fused candidates. Off by default.

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
| RSS at 50k chunks | < 200 MB |
| p95 search latency, 8 concurrent readers | < 30 ms (embedding the query excluded) |
| HNSW recall@10 vs exact | ≥ 0.95 |
| Hybrid recall@10 on the golden set | ≥ lexical-only |
