# Eval and benchmark results (Apple Silicon)

Run with `cargo run --release [--features model2vec] --example eval -- <root> <golden file> <index-dir>`. Queries marked `"broad": true` ("how does sync work?") are also scored as a separate subset, the target of per-community summary chunks (DRAG-31).

## Quality: Dendrite repository (828 files, 5,281 chunks, 30 golden queries)

| embedder | mode | hit@1 | hit@5 | hit@10 | MRR@10 |
|---|---|---|---|---|---|
| — | lexical | 0.57 | 0.77 | 0.83 | 0.663 |
| nomic-embed-text (gateway) | dense | 0.53 | 0.77 | 0.93 | 0.645 |
| nomic-embed-text (gateway) | **hybrid** | **0.67** | **0.87** | **0.97** | **0.771** |
| potion-code-16M-v2 (model2vec) | dense | 0.60 | 0.80 | 0.90 | 0.693 |
| potion-code-16M-v2 (model2vec) | hybrid | 0.50 | 0.87 | 0.93 | 0.672 |

Decision: the default stays nomic through the gateway (best hybrid). model2vec is the offline option: it embeds the corpus in 1.3 s, against 558 s through the gateway.

## Vector index at scale (DRAG-41, 2026-10-22)

`BENCH_FLAT_ONLY=1 cargo run --release --features hnsw --example vector_bench -- 768 50000 100000 200000 400000` (768-d clustered unit vectors, 200 queries, Apple Silicon; recall@10 of the 40-candidate shortlist rescored with exact vectors).

| vectors | index | build | p50, 1 reader | p50, 8 readers | recall@10 |
|---|---|---|---|---|---|
| 20,000 | flat | 25 ms | 1.0 ms | 0.65 ms | 1.000 |
| 20,000 | HNSW ef 64 / 128 / 200 | 275 s | 1.4 / 1.8 / 4.7 ms | 0.7 / 1.5 / 3.3 ms | 0.736 / 0.893 / 0.953 |
| 50,000 | flat | 56 ms | 0.9 ms | 3.0 ms | 1.000 |
| 100,000 | flat | 104 ms | 2.4 ms | 10.6 ms | 1.000 |
| 200,000 | flat | 345 ms | 9.0 ms | 23.5 ms | 1.000 |
| 400,000 | flat | 567 ms | 10.8 ms | 55.0 ms | 1.000 |

Findings:
- The HNSW implementation loses to the flat scan on build time, latency and recall at 20k vectors of 768 dimensions; building it at 100k+ would take hours, so `auto` now always picks flat. Storing each node's vector inline (no id lookup per distance) cut the build to 118 s but changed nothing else, so it was not kept.
- The flat scan meets the 30 ms target with 8 readers up to about 200k vectors (memory-bandwidth bound beyond). Larger corpora need a better approximate index (DRAG-43).

## Fusion weight sweep (DRAG-26, 2026-10-22)

`EVAL_SWEEP=all`: hybrid MRR@10 for lexical ∈ {0.5, 1, 1.5}, dense ∈ {0.5, 1, 1.5, 2}, symbol ∈ {0.5, 1, 1.5, 2}, community ∈ {0, 0.25} on the three golden sets; chosen by the mean over the corpora, with the worst corpus as a check, so no single repository decides.

| embedder | weights (lexical, dense, symbol, community) | mean MRR@10 | worst corpus | mean broad MRR@10 |
|---|---|---|---|---|
| jina-code (candle) | 1, 1, 1.5, 0 (old default) | 0.842 | 0.797 | 0.723 |
| | **1.5, 1, 2, 0 (new default)** | **0.854** | **0.820** | 0.721 |
| model2vec potion-code-16M | 1, 1, 1.5, 0 (old default) | 0.681 | 0.625 | 0.491 |
| | **1, 0.5, 1, 0 (new default)** | **0.738** | **0.650** | **0.572** |

Findings:
- The symbol weight changes nothing between 0.5 and 2 on these sets: symbol-name hits are already found by BM25.
- Corpora disagree for jina: Dendrite (Go) is best dense-heavy (0.5, 1.5: 0.828), the Rust repositories lexical-heavy (1.5, 0.5: up to 0.908). The mean favours a slightly lexical-heavy mix.
- model2vec embeddings need a smaller dense weight: the old weights made hybrid worse than dense alone, the new ones beat both on every corpus.
- The community boost wins on no mean, so it stays off.

## Community summaries (DRAG-31, 2026-10-22)

Same three golden sets and embedder as DRAG-25 (dendrite-rag-rs had grown to 75 files / 382 chunks, so its rows are not comparable with DRAG-25). Indexes rebuilt with `EVAL_FULL=1`.

| corpus | variant | hybrid MRR@10 | broad MRR@10 |
|---|---|---|---|
| Dendrite | baseline (DRAG-25) | 0.810 | 0.747 |
| | summaries in the lexical/dense rankings, expanded to top-symbol chunks | 0.762 | 0.688 |
| | separate summary ranking, expanded, weight 0.5 | 0.779 | 0.664 |
| | community boost ×1.25 on found code | 0.774 | 0.747 |
| | **off (shipped default)** | **0.797** | **0.747** |
| embedag | baseline (DRAG-25) | 0.852 | 0.724 |
| | separate summary ranking, weight 0.5 | 0.812 | 0.724 |
| | community boost ×1.25 | 0.820 | 0.724 |
| | **off (shipped default)** | **0.852** | **0.724** |
| dendrite-rag-rs | separate summary ranking, weight 0.5 | 0.835 | 0.692 |
| | community boost ×1.25 | 0.868 | 0.754 |
| | boost only for queries without identifiers | 0.848 | 0.692 |

Findings:
- Replacing a summary with its community's top-symbol chunks misleads: those chunks are rarely the file a broad question expects.
- Raising code the other rankings already found inside the matched community helps the smallest corpus (+0.06 broad) and hurts the two larger ones on narrow queries, so it ships off and becomes a per-corpus fusion setting (DRAG-26).
- The DRAG-28 schema migration left stale Tantivy documents behind (duplicated hits; lexical MRR 0.569 instead of 0.614 on Dendrite). Fixed: an index run with an empty file table rebuilds the lexical index.

## Three corpora (DRAG-25, 2026-10-21)

Golden sets: `golden-dendrite.json` (37 queries, 8 broad; three queries that named `pkg/rag/indexer.go`, removed by DEN-1044, were retargeted or dropped), `golden-embedag.json` (25 queries, 8 broad) and `golden-dendrite-rag.json` (25 queries, 8 broad). Embedder: `auto` → candle on Metal, `jinaai/jina-embeddings-v2-base-code`.

| corpus | files / chunks | mode | hit@1 | hit@5 | hit@10 | MRR@10 | broad MRR@10 |
|---|---|---|---|---|---|---|---|
| Dendrite (Go) | 833 / 5,286 | lexical | 0.51 | 0.70 | 0.81 | 0.614 | 0.432 |
| | | dense | 0.51 | 0.81 | 0.89 | 0.651 | 0.458 |
| | | **hybrid** | **0.68** | **0.97** | **1.00** | **0.810** | **0.747** |
| embedag (Rust) | 133 / 815 | lexical | 0.68 | 0.96 | 1.00 | 0.824 | 0.825 |
| | | dense | 0.60 | 0.96 | 1.00 | 0.742 | 0.641 |
| | | **hybrid** | **0.76** | **0.96** | **1.00** | **0.852** | 0.724 |
| dendrite-rag-rs (Rust) | 67 / 330 | lexical | **0.84** | 0.96 | 1.00 | **0.894** | **0.889** |
| | | dense | 0.80 | 0.96 | 1.00 | 0.868 | 0.692 |
| | | hybrid | 0.80 | **1.00** | 1.00 | 0.878 | 0.712 |

Findings:
- Hybrid wins clearly on the large Go corpus. On the two small Rust corpora lexical is as good or better, and on their broad questions hybrid trails lexical: the fixed fusion weights do not suit every corpus and embedder (DRAG-26).
- The small corpora saturate hit@10, so they discriminate only at hit@1 and MRR.
- First full index with candle on Metal: 1,047 s for Dendrite (5,286 chunks), 140 s for embedag, 56 s for dendrite-rag-rs; later runs reuse the cache. RSS with the model loaded: about 630 MB.
- int8 vs exact recall@10: 1.000 on all three. p95 vector scan under 25 ms at 5k chunks with 8 readers.

## Embedding backends (DRAG-18 to DRAG-21, 2026-10-21)

Same golden set; Dendrite repository at the time of each run (832–833 files, about 5,290 chunks). Embedding time is the first full index; later runs reuse the cache.

| backend (device) | model | dense MRR | hybrid hit@1 | hybrid hit@10 | hybrid MRR | first embedding | peak RSS |
|---|---|---|---|---|---|---|---|
| ollama (Metal, via llama.cpp) | nomic-embed-text | 0.641 | 0.67 | 0.97 | 0.770 | 8.4 min | 97 MB (model in the Ollama server) |
| candle (CPU) | nomic-embed-text-v1.5 | 0.656 | 0.67 | 0.97 | 0.770 | 45.8 min | 2.2 GB |
| candle (CPU) | jina-embeddings-v2-base-code | 0.716 | 0.67 | **1.00** | **0.807** | 46.3 min | 2.6 GB |
| **candle (Metal)**, `auto` default | jina-embeddings-v2-base-code | 0.654 | 0.67 | 0.97 | 0.806 | 20.6 min | 627 MB |
| oxillama (CPU) | Qwen3-Embedding-0.6B Q8_0 | — | — | — | — | stopped after 5 h without finishing (over 3 s per chunk) | — |

- candle on Metal reproduces the reference PyTorch vectors (cosine 1.0000) and is the `auto` default on GPU builds. It embeds 2.2× faster than candle on the CPU with a quarter of the memory, but Ollama is still 2.5× faster for the first full index.
- jina-embeddings-v2-base-code is the best model measured. The Metal and CPU rows differ by one question because the corpus changed between runs (one file fewer).
- Qwen3-Embedding through oxillama matches the reference model (cosine 0.9996) but runs on the CPU: oxillama-runtime 0.1.4 offloads only Q4_0 single-token decode to the GPU, and embeddings are multi-token prefill. `auto` never picks it.
- Search latency with jina-code vectors (8 concurrent readers): lexical p95 3.1 ms, vector p95 14.5 ms; vector recall@10 against exact search 1.000.

## Scale: `~/go/pkg/mod/github.com` (5,887 files, 49,302 chunks)

| gate | target | measured | status |
|---|---|---|---|
| p95 latency, 8 concurrent readers | < 30 ms | lexical 2.0 ms, vector (rescored) 3.3 ms | pass |
| vector recall@10 vs exact f32 | ≥ 0.95 | 0.967 at 49k / 256-d (0.908 before exact rescoring); 1.000 on Dendrite / 768-d | pass |
| RSS, default configuration (gateway embeddings) | < 200 MB | lexical baseline 96 MB, plus ~37 MB of int8 vectors at 768-d: ~135 MB (derived, not measured) | pass |
| RSS, model2vec in-process | — | 254 MB, of which ~165 MB is the model loaded at open | documented |

How memory came down from 191 MB to 96 MB (lexical, 49k chunks):
- The symbol index interns chunk ids, file paths and symbol names as `u32` handles (+142 MB → +47 MB).
- The redb page cache is bounded to 8 MB (−30 MB, no latency change).
- The embedding phase keeps only `(chunk id, content hash)` pairs and loads texts per group.

The eval prints RSS after each phase to stderr, to make regressions easy to locate.
