# Eval and benchmark results (Apple Silicon)

Run with `cargo run --release -p loom-eval -- quality <root> <golden file>…` (quality, tokens and latency of every method and baseline), `loom-eval probe` (corpus scores per query) and `loom-eval repeat <root> "<question>" --bin target/release/dendrite-rag [--llm]` (one question asked in fresh sidecar processes, timed per stage). The older sections below used `examples/eval.rs` (now `crates/dendrite-rag/examples/eval.rs`); queries marked `"broad": true` are also scored as a subset there.

## Code and documentation, tokens and baselines (loom-rag, 2026-10-22)

Embedder `auto` → Ollama `nomic-embed-text`, every index fully embedded. Golden sets: `golden-dendrite.json` (37 code questions), `golden-dendrite-docs.json` (27 questions answered by `docs/`, new), `golden-dendrite-mixed.json` (12 questions needing a doc and the code it describes, new), `golden-embedag.json` (25), `golden-dendrite-rag.json` (25, paths of the workspace layout), and CoIR CosQA (500 test queries over 20,604 Python functions, one file each). Columns: MRR@10 over files; *recall@B* is the share of queries whose expected file is among the units a reader takes in order while they fit B tokens (whole files for the file baselines); *tokens to answer* is the median read up to the first expected file; *both@2.5k* is the share of mixed questions with an expected doc and an expected code file within 2,500 tokens. A unit counts the files whose identical text it folded (`duplicates`).

Baselines over the same files: **grep-agent** ranks files by the distinct query words they contain and reads them whole (what a grep-driven coding agent reads); **bm25-file** is BM25 over whole files; **naive-chunks** is BM25 over fixed 40-line windows.

| set | method | MRR@10 | nDCG@10 | recall@1k | recall@2.5k | tokens to answer | both@2.5k |
|---|---|---|---|---|---|---|---|
| Dendrite, code | search, code | 0.821 | 0.810 | 0.81 | 0.97 | 484 | 0.00 |
|  | context, all | 0.781 | 0.792 | 0.78 | 0.97 | 599 | 1.00 |
|  | context map, all | 0.781 | 0.792 | 1.00 | 1.00 | 38 | 1.00 |
|  | before: context, all (symbol boost 3, PageRank) | 0.616 | 0.663 | 0.65 | 0.89 | 1,001 | 1.00 |
|  | grep-agent | 0.476 | 0.535 | 0.14 | 0.35 | 15,613 | 0.00 |
|  | bm25-file | 0.527 | 0.584 | 0.16 | 0.38 | 4,885 | 0.00 |
|  | naive-chunks | 0.602 | 0.647 | 0.59 | 0.81 | 731 | 1.00 |
| Dendrite, docs | search, docs | 0.825 | 0.848 | 0.89 | 0.96 | 251 | — |
|  | context, all | 0.741 | 0.737 | 0.78 | 0.85 | 251 | — |
|  | context map, all | 0.741 | 0.737 | 0.89 | 0.89 | 48 | — |
|  | before: context, all | 0.619 | 0.621 | 0.67 | 0.85 | 301 | — |
|  | search, code | 0.000 | 0.000 | 0.00 | 0.00 | — | — |
|  | grep-agent | 0.388 | 0.468 | 0.15 | 0.44 | 21,959 | — |
|  | bm25-file | 0.682 | 0.717 | 0.15 | 0.37 | 3,523 | — |
|  | naive-chunks | 0.734 | 0.757 | 0.74 | 0.85 | 531 | — |
| Dendrite, mixed | search, all | 0.604 | 0.629 | 0.75 | 0.83 | 969 | 0.67 |
|  | context, all | 0.604 | 0.604 | 0.83 | 0.83 | 808 | 0.50 |
|  | context map, all | 0.604 | 0.619 | 1.00 | 1.00 | 81 | 0.67 |
|  | before: context, all | 0.544 | 0.520 | 0.58 | 0.75 | 2,135 | 0.33 |
|  | grep-agent | 0.431 | 0.417 | 0.25 | 0.50 | 30,581 | 0.08 |
|  | bm25-file | 0.688 | 0.735 | 0.25 | 0.50 | 3,124 | 0.17 |
|  | naive-chunks | 0.682 | 0.714 | 0.67 | 0.92 | 550 | 0.58 |
| embedag | search, code | 0.940 | 0.926 | 1.00 | 1.00 | 406 | — |
|  | context, all | 0.833 | 0.816 | 0.84 | 0.96 | 619 | — |
|  | context map, all | 0.833 | 0.841 | 1.00 | 1.00 | 46 | — |
|  | before: context, all | 0.767 | 0.743 | 0.68 | 0.92 | 1,145 | — |
|  | grep-agent | 0.666 | 0.713 | 0.12 | 0.32 | 5,340 | — |
|  | bm25-file | 0.639 | 0.682 | 0.16 | 0.28 | 4,146 | — |
|  | naive-chunks | 0.681 | 0.714 | 0.68 | 0.84 | 625 | — |
| dendrite-rag-rs | search, code | 0.883 | 0.840 | 0.88 | 0.96 | 533 | — |
|  | context, all | 0.768 | 0.748 | 0.84 | 0.92 | 589 | — |
|  | context map, all | 0.768 | 0.754 | 1.00 | 1.00 | 57 | — |
|  | before: context, all | 0.784 | 0.764 | 0.80 | 0.88 | 1,008 | — |
|  | grep-agent | 0.474 | 0.542 | 0.04 | 0.12 | 5,007 | — |
|  | bm25-file | 0.410 | 0.509 | 0.04 | 0.16 | 5,174 | — |
|  | naive-chunks | 0.586 | 0.625 | 0.68 | 0.72 | 1,581 | — |
| CoIR CosQA | search, dense | 0.425 | 0.491 | 0.70 | 0.73 | 193 | — |
|  | search, hybrid | 0.360 | 0.426 | 0.64 | 0.70 | 246 | — |
|  | search, lexical | 0.243 | 0.283 | 0.41 | 0.44 | 224 | — |
|  | grep-agent | 0.154 | 0.179 | 0.25 | 0.31 | 1,557 | — |
|  | bm25-file | 0.261 | 0.291 | 0.40 | 0.50 | 390 | — |
|  | naive-chunks | 0.259 | 0.290 | 0.39 | 0.49 | 406 | — |

Findings:
- **Against the baselines.** On code questions (Dendrite, embedag, dendrite-rag-rs) every engine method beats every baseline on MRR, by 0.22 to 0.41 over grep-agent and 0.15 to 0.47 over BM25 on whole files; on documentation questions the docs pipeline leads (0.825 against 0.734 for 40-line windows). On mixed questions BM25 over whole files and over windows rank the first expected file higher (0.688 and 0.682 against 0.604) because a whole file holds the doc and the code it is about at once, but they read 3,124 and 550 tokens to get there and keep both files within 2,500 tokens for 0.17 and 0.58 of the questions, against 0.67 for an `all` search or outline map. The token difference is large everywhere: a grep-driven agent reads a median 1,557 to 30,581 tokens of whole files before the answer's file, a context pack 251 to 808, an outline map 38 to 81, and within 1,000 tokens the map names the answer's file for every code and mixed question (recall 1.00, against 0.04 to 0.25 for grep-agent). On CoIR CosQA (natural-language questions over Python functions) dense retrieval is best (nDCG@10 0.491) and beats every baseline; lexical search is level with BM25 over files (0.283 against 0.291) and hybrid lands between (0.426): BM25 and symbol names pull short natural-language questions toward functions that share a word, so the fusion weights should depend on the question (follow-up).
- **Code and documentation are different, and the question decides.** The code pipeline beats `all` on code questions by 0.05 to 0.12 MRR on all three repositories; the docs pipeline beats `all` on documentation questions by 0.08; `all` is the only corpus that answers mixed questions (both files within 2,500 tokens for 0.67 of them). No automatic rule found the corpus from the question (`loom-eval probe`): neither each corpus's best cosine similarity, nor its best BM25 score, nor the rank-weighted share of documentation among the best `all` candidates separates code questions from documentation questions without sending documentation questions to code alone, where they score 0. Two `auto` modes were built and measured: weighting the pipelines against each other (at symbol boost 1.5: MRR 0.71 against 0.76 for `all` on code questions, 0.64 against 0.76 on documentation) and narrowing `all` to one corpus when the other was absent from its top ten (within 0.02 of `all`); neither shipped. `dendrite ask` therefore searches code unless `-d` adds the documentation, and the RPC default stays `all`.
- **Context packs were worse than plain search; three fixes, each measured.** (1) The personalized PageRank re-weighting of spans moved central files above the answer: dropping it alone raised code-question MRR of code packs from 0.651 to 0.834 and recall within 2,500 tokens from 0.92 to 1.00 (Dendrite, symbol boost 1.5). (2) The symbol-name boost of 3 let natural-language words that name functions outrank answering text: a sweep over 1.5 / 2 / 2.5 / 3 picked 2.5, the best mean over the five sets for `all` (search 0.743, context 0.742; 1.5 is best for documentation questions alone, 0.762, but costs the Rust repositories 0.05 to 0.08). (3) Identical text (vendored or copied files, 9 skill files duplicated across `.agents/` and `.gemini/` in Dendrite, many CosQA functions) is returned once, with the copies' paths in `duplicates`. Together, `all` context packs gained 0.06 to 0.17 MRR on four of five sets and read 17 to 62 % fewer tokens to the answer; dendrite-rag-rs lost 0.016 (0.784 to 0.768), where `code` scores 0.883. On CosQA, where many functions are identical, context packs rose from 0.051 to the 0.243 of search.
- **Outlines.** `detail: map` returns each span's doc comment or heading and declaration (`outline: true`), median 38 to 81 tokens to the answer's file against 251 to 808 for full text: the token-cheapest first step for an agent, which then fetches bodies with `chunks.get` / `dendrite_rag_show`. `detail: auto` (the default) outlines spans pulled in only through the symbol graph; on these budgets the direct hits fill the pack first, so it measures the same as `full`.
- Doc-to-code links (a documentation section naming a symbol or path in inline code pulls that code in) were built and measured: no change on the mixed set, whose documentation names commands rather than identifiers; not shipped.
- **Mixed context packs keep 20% for documentation** (2026-10-22). With `-d`, Dendrite's AGENTS.md ranked 7th in the one `all` ranking and six code chunks filled the 2,500 tokens before it. `all` packs now run each pipeline on its own corpus: the docs pack gets up to 20% of the budget, code the rest, and either takes room the other leaves; the spans interleave by the `all` ranking, because alternating them put code first on documentation questions (MRR 0.40). `context auto / all`, before → after (MRR, recall@2.5k): Dendrite code 0.759, 0.95 → 0.738, 0.97; docs 0.747, 0.85 → 0.725, 0.78; mixed 0.604, 0.92 → 0.614, 0.83, both@2.5k 0.50 → 0.75. A 30% share scored code recall@2.5k 0.89 and 40% scored 0.92; 20% was kept because answers should lean on code, which documentation can contradict.

### The same question twice, timed per stage

`loom-eval repeat <copy of Dendrite> "how does the token gateway work?" --llm`: each run starts a fresh `dendrite-rag`, indexes with the interactive 3 s embedding budget, packs the context and answers with the gateway's `local-coder`.

| index | binary | run | sidecar start | index (embedding) | context | local model | total |
|---|---|---|---|---|---|---|---|
| 0 of 5,286 chunks embedded (the real Dendrite index) | develop | 1 | 11 ms | 3,262 ms (3,013) | 81 ms | 38,328 ms | 41.7 s |
| | | 2 | 14 ms | 3,420 ms (3,189) | 64 ms | 33,619 ms | 37.1 s |
| | loom-rag, focus | 1 | 12 ms | 3,314 ms (3,062) | 57 ms | 39,520 ms | 42.9 s |
| | | 2 | 14 ms | 3,440 ms (3,292) | 65 ms | 24,644 ms | 28.2 s |
| fully embedded | loom-rag | 1 | 5 ms | 80 ms (1) | 242 ms | 30,775 ms | 31.1 s |
| | | 2 | 12 ms | 360 ms (1) | 104 ms | 27,347 ms | 27.8 s |
| fully embedded, answer cache | loom-rag | 1 | 441 ms | 130 ms (1) | 321 ms | 41,482 ms | 42.4 s |
| | | 2 | 13 ms | 300 ms (1) | 85 ms | 0 ms (cached) | **0.4 s** |

Findings:
- Retrieval was used and is not the slow part: packing the context takes 57 to 321 ms. The local model takes 25 to 41 s per answer. With an embedding backlog every call also spends its whole 3 s budget embedding about 48 chunks (the real Dendrite index had 0 of 5,286 vectors), and dense retrieval sees almost nothing; `status` now reports `embed_coverage` and a warning, and `dendrite ask` prints the coverage with the command that finishes the backlog (6.8 minutes for Dendrite on Ollama). With `focus`, the chunks the question matches lexically are embedded first.
- The same question about unchanged code is answered from the ledger's answer cache (keyed by model and exact prompt): 0.4 s instead of 42 s. `fresh: true` (`dendrite ask --update-index`) asks the model again.
- No answer cache existed before, so asking twice cost the model twice.

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

## External validity and Qwen3-Embedding (DRAG-52, 2026-10-24)

**CoIR CosQA, reproducible.** `uv run --no-project --with pyarrow python -I eval/coir/import.py cosqa <dir> --ext py --queries 500` materializes the corpus (20,604 Python functions, one file each) and 500 test queries from the Hugging Face Parquet files; lexical search scores MRR@10 0.243 [0.211, 0.277], nDCG@10 0.282, the same as the earlier hand-built run (0.243 / 0.283). BM25 there: q-log IDF is significantly worse (q = 0.5: -0.047 MRR [-0.066, -0.030]; q = 0.1: -0.074 [-0.098, -0.051]), k1 = 1.5 is neutral (+0.003 [-0.000, 0.007]): the DRAG-47 defaults hold on an external benchmark too.

**Qwen3-Embedding-0.6B** on candle (Metal) against `nomic-embed-text` (Ollama), fresh indexes, `loom-eval compare`, MRR@10:

| set | dense | hybrid | index time |
|---|---|---|---|
| dendrite-rag-rs (721 chunks) | -0.090 [-0.219, 0.026] | +0.007 [-0.117, 0.137] | 229 s against 55 s |
| embedag (826 chunks) | +0.048 [-0.070, 0.177] | +0.007 [-0.053, 0.067] | 246 s against 84 s |

No significant difference at three to four times the indexing cost: nomic stays the default for Ollama and jina-code for candle. Not done: paid embedders (no gateway key in this environment; configuration only, embedders.md) and a SWE-bench localization slice.

## LLM contextual chunk prefixes: go/no-go (DRAG-48, 2026-10-24)

Contextual retrieval with an LLM (a 50-100 token text situating each chunk in its file, indexed with the chunk and never returned) against the no-LLM header the index already has (path and scope). Prefixes by Gemini 3.8 Flash through `agy` (`eval/contextual/gen.py`: one call per file with the whole document and its chunks; files of one chunk under 2,000 characters skipped): 593 of 672 chunks of dendrite-rag-rs (116 calls, 22 min) and 669 of 826 of embedag. Fresh indexes with and without them, `loom-eval compare`, MRR@10:

| set | lexical search | hybrid search (`nomic-embed-text`) | hybrid context pack |
|---|---|---|---|
| dendrite-rag-rs | +0.004 [-0.056, 0.060] | +0.049 [-0.093, 0.195] | +0.042 [-0.108, 0.192] |
| embedag | +0.027 [0.000, 0.080] | -0.030 [-0.073, 0.000] | -0.030 [-0.073, 0.000] |

Dense search alone moved -0.063 (dendrite-rag-rs) and +0.014 (embedag); on embedag the hybrid nDCG@10 fell, -0.025 [-0.057, -0.002]. **No-go:** the gate (+0.02 MRR with a CI above 0 on two of the three code sets) fails on both sets measured, so the Dendrite set (915 files) was not generated and the enrichment pipeline (outbox, worker, enrich RPC: DRAG-50) is not built. The reported gains of contextual retrieval came from prose corpora; code chunks here already carry their path and scope, and the extra text dilutes the embedding as often as it helps. `Chunk.context`, `Config::chunk_contexts`, `loom-eval export-chunks` and `--contexts` stay for a later experiment (another generator, a reranker, larger corpora).

## Abstention and the outline fallback (DRAG-51, 2026-10-24)

**Abstention.** `loom-eval abstain-probe` with the in-process `BAAI/bge-reranker-base` (candle, Metal), `nomic-embed-text` vectors, the golden sets as answerable questions and `eval/golden-negatives.json` (8 questions per repository naming features the repository does not have, checked by grep). Shares abstained at each threshold of the best hit's relevance probability:

| threshold | Dendrite: unanswerable / answerable | embedag | dendrite-rag-rs |
|---|---|---|---|
| 0.20 | 0.75 / 0.08 | 0.50 / 0.00 | 0.62 / 0.00 |
| 0.30 | 1.00 / 0.12 | 0.75 / 0.00 | 0.62 / 0.00 |
| 0.45 | 1.00 / 0.18 | 0.88 / 0.00 | 0.88 / 0.04 |
| 0.50 | 1.00 / 0.18 | 1.00 / 0.00 | 0.88 / 0.08 |

The gate (≥ 0.8 of unanswerable questions, ≤ 0.05 of answerable ones) holds at 0.45 for embedag and dendrite-rag-rs and at no threshold for Dendrite (76 questions, many about its documentation, which the reranker scores as low as unanswerable questions). Abstention therefore ships off, set per project from the probe.

**Outline fallback.** Context packs, lexical only, before and after (`loom-eval compare`): MRR@10 within ±0.014 on every set and method (no CI excludes 0); recall within 1,000 tokens up for `context auto / all` on every set (Dendrite 0.65 → 0.78, Dendrite docs 0.59 → 0.89, mixed 0.75 → 0.92, embedag 0.92 → 1.00, dendrite-rag-rs 0.72 → 0.84), and recall within 2,500 and 4,000 tokens never lower. It is on.

## BM25 parameters and q-log IDF (DRAG-47, 2026-10-24)

`[lexical] k1, b, q` (`DENDRITE_RAG_BM25_*`) now reach Tantivy (a bag-of-terms scorer over its postings when they differ from Tantivy's built-in 1.2 / 0.75), the PostgreSQL BM25 and `rank`. Sweep: k1 ∈ {0.8, 1.0, 1.2, 1.5} × b ∈ {0.3, 0.45, 0.6, 0.75} × q ∈ {1, 0.5, 0.2, 0.1, 0.05}, lexical only (embedder `none`), the five golden sets, `loom-eval quality --json` per configuration and `loom-eval compare` for paired 95 % CIs. MRR@10 of `search lexical / all`; "code" is the mean of Dendrite, embedag and dendrite-rag-rs.

| k1 / b / q | code | Dendrite | embedag | dendrite-rag-rs | docs | mixed |
|---|---|---|---|---|---|---|
| 1.2 / 0.75 / 1 (default) | 0.697 | 0.609 | 0.833 | 0.648 | 0.393 | 0.669 |
| same, bag-of-terms scorer | 0.697 | — | — | — | 0.393 | 0.669 |
| 1.2 / 0.75 / 0.5 | 0.688 | 0.501 | 0.920 | 0.643 | 0.069 | 0.388 |
| 1.2 / 0.75 / 0.05 | 0.622 | 0.384 | 0.903 | 0.580 | 0.012 | 0.240 |
| 1.5 / 0.75 / 1 | 0.702 | | | | 0.440 | 0.669 |
| 1.2 / 0.45 / 1 | 0.699 | | | | 0.300 | 0.635 |
| 0.8 / 0.75 / 1 | 0.710 | | | | 0.359 | 0.700 |

Findings:
- **The custom scorer is exact.** At Tantivy's parameters it ranks every set identically to Tantivy's own scorer (unit test: scores within 0.1 %), and PostgreSQL ranks alike with configured parameters (`configured_bm25_ranks_alike_in_sql_and_tantivy`).
- **q-log IDF hurts here.** It helps only embedag (0.833 → 0.920 at q = 0.5) and costs Dendrite 0.11 to 0.23 MRR and documentation almost everything (0.393 → 0.012): sharpening rare terms lets one rare word in a natural-language question outrank the passage that answers it. The reported CoIR-Go gain does not carry over to these paraphrased questions. q stays 1.
- **Lower b does not help code** (0.697 → 0.699 at b = 0.45) and costs documentation 0.09; b stays 0.75.
- **k1 = 1.5** is the only significant change: documentation questions in lexical search +0.048 MRR [0.006, 0.106]; code sets +0.000 to +0.020, every CI spanning 0. With the full hybrid pipeline (Dendrite, `nomic-embed-text`, 100 % vector coverage) no method × set row of 38 changes significantly except that same lexical docs search (+0.048 [0.001, 0.119]). The defaults stay; the settings are there for projects whose own sweep says otherwise.

, 2026-10-24)

`cargo bench -p loom-rag --features testutil --bench retrieval` (criterion, Apple Silicon): a synthetic Go project of 2,000 files (`testutil::synthetic_project`), the 64-d `HashEmbedder`, each query-path figure covering the same four queries.

| benchmark | time |
|---|---|
| `query/lexical` (4 queries, top 10) | 320 µs |
| `query/dense` | 1.13 ms |
| `query/hybrid` | 1.44 ms |
| `query/context_2500` (context packs) | 5.75 ms |
| `flat_scan_50k_256d_top40` (int8 scan, one query) | 195 µs |
| `rrf_3x100` (fusing three lists of 100) | 10.4 µs |

`cargo test --release -p loom-rag --features dhat,testutil --test peak_heap`: a cold index of 4,000 synthetic files peaks at 61.2 MB of heap (dhat), most of it the fixed 50 MB Tantivy writer budget; the test fails above 96 MB and runs in CI (`memory` job). These are the baseline the BM25 (DRAG-47) and storage (DRAG-49) changes are compared with.

## Cold local index at 49k chunks (DRAG-42, 2026-10-22)

`cargo run --release --example graph_stats -- ~/go/pkg/mod/github.com <index-dir>` (5,887 files, 49,302 chunks, no embedder: what an edge-mode checkout rebuilds locally while its embeddings come from the shared tier).

| run | wall time | of which graph + communities | peak RSS |
|---|---|---|---|
| cold (empty index) | 14.1 s | 3.4 s (74,373 symbols, 1,631 communities) | 463 MB |
| no-op re-run | 2.8 s | 2.6 s graph rebuild in memory | — |

Findings:
- A cold local rebuild of a 50k-chunk corpus takes 14 s, under the 30 s set in DRAG-42, against about 17 minutes to embed the same corpus on candle Metal (scaled from the Dendrite run). Edge mode therefore keeps sharing embeddings only; pulling index segments would pay off from roughly ten times this size and is not built.
- Peak memory is 463 MB, over the 200 MB budget: the code graph build holds every chunk with its text (DRAG-44).

### Cold index memory after DRAG-44 (2026-10-24)

Same corpus and command, `/usr/bin/time -l`, re-measured before and after on the same machine (the baseline is higher than the 463 MB above). Ranges are over repeated runs.

| run | wall time | max RSS | peak memory footprint |
|---|---|---|---|
| before | 8.5 s | 714 MB | 556 MB |
| code graph streamed from the ledger (`GraphBuilder`) | 8.7 s | 442 MB | 321 MB |
| + files chunked and written 256 at a time, one commit | 8.0–11.4 s | 389–395 MB | 227–230 MB |
| + both stores committed per group of 256 files | 12.9–14.1 s | 376–398 MB | 201–208 MB |
| **+ groups of 512 files (shipped)** | 10.4–10.9 s | 420–446 MB | 179–202 MB |

Findings:
- RSS probes per stage put the peak at chunking: every chunk of a cold index (49k, with text and up to 64 refs each) was held at once until both stores were written, and the graph build then collected them all again. Chunking and committing a group of files at a time, and streaming the ledger into the graph, cut the peak footprint (the process's dirty memory) by 64 %, to the 200 MB budget.
- Max RSS stays near 400 MB: it also counts Tantivy's memory-mapped segment files, which grow and merge during the run, and pages the allocator keeps after freeing them; neither is the process's own working memory. The budget is checked against the footprint.
- Per-group commits cost fsyncs: 512-file groups take about 2 s more than one commit, 256-file groups about 4 s.
- A crash between groups leaves the committed groups current. Each file is removed from the lexical index by path before its chunks are added, and a modified file's ledger entry is removed with its chunks, so the next run re-chunks exactly the uncommitted files without duplicates (`a_group_committed_only_to_the_lexical_index_is_not_duplicated`).

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
