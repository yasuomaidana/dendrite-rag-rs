# Eval and benchmark results (DRAG-15, 2026-10-21, Apple Silicon)

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

## Scale: `~/go/pkg/mod/github.com` (5,887 files, 49,302 chunks)

| gate | target | measured | status |
|---|---|---|---|
| p95 latency, 8 concurrent readers | < 30 ms | lexical 1.9 ms, vector (rescored) 3.4–4.7 ms | pass |
| vector recall@10 vs exact f32 | ≥ 0.95 | 0.967 (int8 shortlist ×4, exact rescoring; 0.908 without rescoring) | pass |
| RSS | < 200 MB | lexical-only 191 MB; model2vec in-process 290–354 MB (model weights included) | **open** |

With gateway embeddings at 768 dimensions, the int8 vectors add about 37 MB to the lexical baseline, an estimated ~230 MB. That is not yet measured and is over the target. Next steps: trim the symbol index (intern names), cap Tantivy reader caches, and memory-map `vectors.bin`.
