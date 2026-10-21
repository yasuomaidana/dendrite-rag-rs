# Eval and benchmark results (Apple Silicon)

Run with `cargo run --release [--features model2vec] --example eval -- <root> eval/golden-dendrite.json <index-dir>`.

## Quality: Dendrite repository (828 files, 5,281 chunks, 30 golden queries)

| embedder | mode | hit@1 | hit@5 | hit@10 | MRR@10 |
|---|---|---|---|---|---|
| — | lexical | 0.57 | 0.77 | 0.83 | 0.663 |
| nomic-embed-text (gateway) | dense | 0.53 | 0.77 | 0.93 | 0.645 |
| nomic-embed-text (gateway) | **hybrid** | **0.67** | **0.87** | **0.97** | **0.771** |
| potion-code-16M-v2 (model2vec) | dense | 0.60 | 0.80 | 0.90 | 0.693 |
| potion-code-16M-v2 (model2vec) | hybrid | 0.50 | 0.87 | 0.93 | 0.672 |

Decision: the default stays nomic through the gateway (best hybrid). model2vec is the offline option: it embeds the corpus in 1.3 s, against 558 s through the gateway.

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
