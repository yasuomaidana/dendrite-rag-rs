# Retrieval

## Lexical index (Tantivy BM25)

Fields:

| Field | Tokenizer | Boost |
|---|---|---|
| `text` (header + content) | code tokenizer | 1.0 |
| `symbols` | code tokenizer | 2.5 |
| `path` | code tokenizer | 1.5 |
| `chunk_id` | raw, stored | — |

**Code tokenizer.** Each token is split on non-alphanumerics, then on case and digit boundaries (`parseHTTPResponse2` → `parse`, `http`, `response`, `2`). Everything is lowercased. The whole identifier (`parsehttpresponse2`) is also emitted, so exact identifier queries score higher than scattered sub-word matches.

**BM25 parameters** (`[lexical] k1`, `b`, `q`; `DENDRITE_RAG_BM25_K1`, `_B`, `_Q`; DRAG-47) default to k1 = 1.2, b = 0.75 and q = 1. The IDF is `ln_q(1 + (N - df + 0.5) / (df + 0.5))` with the q-logarithm `ln_q(x) = (x^(1-q) - 1) / (1 - q)`, the natural logarithm at q = 1 (`src/bm25.rs`, shared with PostgreSQL and `rank`). At the defaults Tantivy's own scorer runs; other values score the query's distinct code terms in each field with the same formula over Tantivy's postings and field norms (`lexical/scorer.rs`), as the PostgreSQL BM25 does. The sweep (eval/RESULTS.md, DRAG-47) found no setting that improves code questions significantly; q < 1 and b < 0.75 hurt documentation questions.

The `symbols` boost also decides how code competes with documentation, which defines no symbols, when both share a ranking. It was 3.0 until the corpus evaluation (eval/RESULTS.md, 2026-10-22). Higher values let natural-language words that happen to name a function (`test`, `check`) outrank the text that answers; lower ones let documentation crowd out code. A sweep over 1.5, 2, 2.5 and 3 on the five golden sets picked 2.5, the best mean MRR for both corpora in one ranking (0.743 for `search`, 0.742 for `auto` context packs).

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
- `[index] vector_index = "auto" | "flat" | "hnsw"` (`DENDRITE_RAG_VECTOR_INDEX`); `auto` uses the flat scan (DRAG-41: HNSW did not beat it at any size it could be built); `hnsw` selects the graph explicitly. `[index] hnsw_ef_search` (`DENDRITE_RAG_HNSW_EF_SEARCH`, default 128) trades recall for speed.
- The graph persists to `vectors.hnsw` ([store.md](store.md)); a missing or corrupt file is rebuilt from `vectors.bin`. Deleted vectors are tombstones until they exceed 20% of the nodes, then the graph is rebuilt.
- Measured (`recall_report`, 20,000 unit vectors of 64 dimensions, 200 queries, Apple Silicon, release build): recall@10 of the engine's 40-candidate shortlist rescored with exact vectors is 0.824 / **0.954** / 0.990 at ef_search 64 / **128** / 200 on uniform random vectors, and 0.908 / **0.974** / 0.991 on clustered ones, which resemble real embeddings more. At this size the parallel flat scan is faster (62 µs per query against 145–210 µs at ef_search 128); the flat scan grows linearly, so the break-even is estimated near 100,000 vectors, but the 768-dimension benchmark below showed otherwise.
- Measured at real embedding size (DRAG-41, `examples/vector_bench.rs`, 768-d clustered vectors, 200 queries, 40-candidate shortlist rescored): at 20,000 vectors HNSW took 275 s to build (single-threaded), answered in 1.4–4.7 ms against 1.0 ms for the flat scan, and reached recall 0.736 / 0.893 / 0.953 at ef_search 64 / 128 / 200 against 1.000. The flat scan keeps recall 1.000 and grows linearly: p50 0.9 / 2.4 / 9.0 / 10.8 ms for one reader and 3.0 / 10.6 / 23.5 / 55.0 ms with 8 concurrent readers at 50k / 100k / 200k / 400k vectors, so it meets the 30 ms target up to about 200,000 vectors. Beyond that an approximate index is needed, but not this one: DRAG-43 replaces or rewrites it (parallel build, contiguous graph vectors) and benchmarks again.

## Symbol-name ranking

Query tokens that look like identifiers (contain `_`, `::`, `.` or an inner capital, or match a known symbol) are looked up in the symbol table. Chunks that define the symbol rank first, then chunks that reference it.

## Fusion

Weighted Reciprocal Rank Fusion over the ranked lists:

`score(d) = Σ_i w_i / (k + rank_i(d))`, with k = 60. The weights depend on the embedder, from the weight sweep over the three golden sets (eval/RESULTS.md, DRAG-26), and `[fusion]` in `.dendrite/rag.toml` (`lexical`, `dense`, `symbol`, `community`; `DENDRITE_RAG_FUSION_*`) overrides any of them:

| Embedder | lexical | dense | symbol | community |
|---|---|---|---|---|
| model2vec (static embeddings) | 1.0 | 0.5 | 1.0 | 0 |
| jina-embeddings-v2-base-code | 1.5 | 1.0 | 2.0 | 0 |
| any other | 1.0 | 1.0 | 1.5 | 0 |

`community` raises code inside the communities whose summaries match best ([graph.md](graph.md) § Summary chunks); it is off everywhere because no corpus gained from it on average. `EVAL_SWEEP=1 cargo run --release --example eval -- …` repeats the sweep for another corpus or embedder.

RRF uses ranks only, so BM25 and cosine scores never need calibrating against each other. Each list contributes its top `candidates` (default 50).

## Optional rerank

A `Reranker` trait scores `(query, chunk)` pairs with a cross-encoder. It is applied to the top 30 fused candidates. Off by default.

Backends (`[rerank] backend`):
- `gateway` (default): server with a Cohere-style `/v1/rerank` endpoint (e.g. LiteLLM gateway, model `bge-reranker-v2-m3`). Off when `model` is unset.
- `candle`: in-process cross-encoder on candle (feature `candle`, default model `BAAI/bge-reranker-base`). Checkpoints with `model_type` `xlm-roberta` (the BGE rerankers) run through candle's XLM-RoBERTa sequence classifier, and `bert` ones (MiniLM-style cross-encoders) through BERT with its pooler and linear head. Runs on the GPU (Metal, CUDA) or CPU. Its logits match the reference implementation (transformers) within 0.05 on `BAAI/bge-reranker-base` (`live_candle_bge_reranker_scores`).
- `none`: reranking disabled.

**Abstention** (`[rerank] abstain`, `DENDRITE_RAG_RERANK_ABSTAIN`, DRAG-51; off by default). With a reranker, a search whose best reranked hit is less likely than the threshold to be relevant returns no hits, and a context pack whose every search abstained comes back empty with `abstained: true`, as does `ask` (no model is called): an agent gets "the project does not cover this" instead of distractors. Gateway scores are already probabilities; candle logits go through the logistic function. Without a reranker it never abstains (no threshold on fused, BM25 or cosine scores separated answerable questions, § Corpora). A threshold is project-specific: `loom-eval abstain-probe <root> <golden…> --negatives eval/golden-negatives.json --set <name>` prints how each threshold splits the golden questions and the unanswerable ones. Measured with `bge-reranker-base` (eval/RESULTS.md): 0.45 abstains on 88 % of unanswerable questions while abstaining on at most 4 % of answerable ones for embedag and dendrite-rag-rs, but no threshold works for Dendrite, whose documentation questions score as low as unanswerable ones, so there is no default.

## Corpora

Every chunk belongs to one corpus, derived from its path and language (`crates/loom-rag/src/corpus.rs`), so no store keeps an extra column:
- **docs**: Markdown, `.txt` and `.rst` files.
- **code**: everything else, including the code and configuration files indexed as plain text (`.kt`, `.sh`, `.sql`, `.toml`, …) and community summary chunks.

Each corpus has its own pipeline over the shared stores:
- **code** (`retrieve/code.rs`): lexical, dense and symbol-name rankings fused as in § Fusion, the community boost, and context packs expanded through the symbol graph (§ Context packing).
- **docs** (`retrieve/docs.rs`): lexical and dense rankings fused with the project's `lexical` and `dense` weights. Prose defines no symbols, so there is no symbol ranking or expansion. A context pack is the best sections, each keeping its heading path as `scope`.

`search`, `context` and `ask` take `corpus`: `all` (the default), `code` or `docs`. `search` over `all` ranks both corpora in one ranking (the code pipeline over every chunk); `context` and `ask` packs over `all` run each pipeline on its own corpus and keep `docs_share` of the budget for documentation (§ Mixed packs). Dendrite's `dendrite ask` searches code unless `-d` (`all`) or `--corpus` is given.

The corpus is the caller's choice because no rule found on the golden sets routes a question to the right corpus by itself (`loom-eval probe`, `retrieve/route.rs`). Neither the best dense similarity of each corpus, nor its BM25 score, nor the rank-weighted share of documentation among the best candidates of both separates code questions from documentation questions: every threshold that keeps documentation questions out of a code-only search also keeps most code questions in `all`. Weighting the two pipelines against each other (measured at symbol boost 1.5) ranked the other corpus's answer too low (MRR 0.71 against 0.76 for `all` on code questions, 0.64 against 0.76 on documentation questions), and narrowing `all` to one corpus only when the other was absent from the top ten matched `all` within 0.02. A fixed corpus costs the other kind of question everything (MRR 0 for documentation questions under `code`), while `code` is worth 0.05 to 0.12 MRR over `all` on code questions:

| Golden set | `code` | `docs` | `all` |
|---|---|---|---|
| Dendrite, code questions | 0.821 | 0.027 | 0.768 |
| Dendrite, documentation questions | 0 | 0.825 | 0.741 |
| Dendrite, mixed (both files within 2,500 tokens) | 0 | 0 | 0.67 |
| embedag | 0.940 | 0 | 0.833 |
| dendrite-rag-rs | 0.883 | 0 | 0.768 |

A filtered ranking fetches 4× its candidates and keeps the first ones in the corpus. Results report `timings` (milliseconds for the lexical, dense, symbol, rerank and expansion stages, and the total).

## Mixed packs

An `all` context pack (`retrieve/mixed.rs`) is a documentation pack within `docs_share` of the budget and a code pack (code pipeline, `code` corpus) in the rest; room the documentation leaves goes to code, and room code leaves goes back to documentation. The spans keep each pipeline's order and interleave by the `all` ranking: the pipelines' scores are not comparable, and alternating them put code first on documentation questions (MRR 0.40). `docs_share` is 0 to 1: the request's, else the project's `[ask] docs_share` (`DENDRITE_RAG_ASK_DOCS_SHARE`), else 0.2, which leans on code because documentation can contradict the implementation. One mixed ranking let six code chunks fill 2,500 tokens before Dendrite's AGENTS.md (7th); the share was measured at 0.2, 0.3 and 0.4 (eval/RESULTS.md).

## Duplicate text

A hit whose text (trimmed) a better hit already holds is dropped, and its path is added to the better hit's `duplicates`: vendored or copied files are read once.

## Context packing

`context(query, token_budget, corpus, detail)`:

1. Take the 20 best fused hits of the corpus (§ Corpora).
2. **Expand** (code pipeline): for each of the 8 best hits, add up to 4 chunks linked in the symbol graph (definitions of the symbols it references, callers of the symbols it defines), each scored with the parent's score × 0.5, × 0.5 again when it is in another community.
3. **Remove overlap**: drop chunks fully contained in a higher-ranked chunk's line span.
4. **Pack** greedily by score until the token budget is reached; a span that does not fit is skipped and smaller ones after it still go in. Tokens are estimated at ~4 characters per token with a 1.15 safety factor, plus 8 for the citation; callers that need exact counts pass a smaller budget.
5. **Merge** spans that touch within a file, so the prompt reads continuous text (outlines are never merged).

Spans keep their fused order. The personalized PageRank re-weighting over the file graph (the repository-map technique) that used to follow step 2 moved central files above the answer: dropping it raised MRR on code questions from 0.65 to 0.83 and recall within 2,500 tokens from 0.92 to 1.00 (eval/RESULTS.md).

`detail` sets how much of each span the pack carries:

| `detail` | Spans |
|---|---|
| `auto` (default) | the text of retrieved hits; spans pulled in only through the symbol graph as outlines |
| `full` | the text of every span |
| `map` | outlines only, for callers that read the map first and fetch bodies with `chunks.get` |

An outline is a span's first two meaningful lines (its doc comment or heading, and its declaration or first sentence; package, import and front-matter lines skipped), each cut to 160 characters, marked `outline: true`. On the Dendrite sets a `map` pack names the answer's file within a median 49 tokens, against 599 for `auto` and about 15,600 for a grep-driven agent reading whole files.

The result lists the spans in score order, each with `chunk_id`, `path`, `start_line`, `end_line`, `scope`, `score`, `content`, `duplicates?` and `outline?`, plus the total estimated tokens and `timings`.

**Outline fallback** (DRAG-51). A span whose text no longer fits the budget is replaced by its outline (doc comment and declaration, `outline: true`) when that fits, instead of being dropped: the pack still names the file, and the agent can fetch the body with `chunks.get`. Measured on the five golden sets (lexical): MRR unchanged, recall within 1,000 tokens up by 0.05 to 0.30 (Dendrite documentation questions 0.59 → 0.89), recall within 2,500 and 4,000 tokens never lower.

## Embedding backlog

An index run under an embedding budget (`embed_budget_ms`) stops embedding when the budget runs out; later runs continue. With `focus` (the question about to be asked), the chunks the question matches lexically (up to 64) are embedded first, so dense retrieval covers them on the first call. `status` reports `embed_coverage` (vectors ÷ chunks) and a warning under 0.9.

## Performance targets

| Metric | Target |
|---|---|
| RSS at 50k chunks | < 200 MB (measured 96 MB lexical baseline at 49k; ~135 MB with 768-d vectors) |
| p95 search latency, 8 concurrent readers | < 30 ms (embedding the query excluded) |
| Quantized recall@10 vs exact | ≥ 0.95 |
| Hybrid recall@10 on the golden set | ≥ lexical-only |

## Ranking supplied documents

`rank` (DRAG-39) ranks documents the caller sends, without a project index: Okapi BM25 (k1 1.2, b 0.75) over the code tokenizer's terms of those texts, cosine similarity with the project's embedder, and the project's fusion weights in RRF (k = 60). Document embeddings go to the same cache as chunk embeddings, keyed by model and content hash, so a fixed catalogue is embedded once; only the query is embedded per call. Without a reachable embedder the ranking is lexical. Dendrite ranks its agent catalogue this way (`dendrite bouton search` / `route`), keeping its own cost, locality and history rules on top.
