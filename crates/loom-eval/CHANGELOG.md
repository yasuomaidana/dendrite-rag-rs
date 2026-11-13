# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-02

### Added
- **Retrieval Quality & Token Benchmarking (`quality`)**:
  - Evaluation harness scoring Hit@1, Hit@5, Hit@10, MRR@10, and nDCG@10 across golden datasets.
  - Token economics evaluation: recall@1k, recall@2.5k, and recall@4k tokens, median tokens to answer, and `both@2.5k` for multi-hop queries.
  - Baselines: `grep-agent` (whole-file iterative grep model), `bm25-file` (whole-file BM25), and `naive-chunks` (fixed 40-line chunk windows).
- **Statistical Significance & Comparisons (`compare`)**:
  - 95% bootstrap percentile confidence intervals for MRR@10 and nDCG@10.
  - Paired bootstrap difference hypothesis testing between baseline and candidate JSON runs (`loom-eval compare`).
- **SWE-bench Verified Runner (`swebench`)**:
  - Incremental indexing and file localization evaluation on SWE-bench Verified across 12 Python repositories (`[DRAG-54]`, `[DRAG-55]`).
  - Stratified-120 sample subset, leakage class categorization (`path_explicit`, `traceback_symbol`, `opaque_nl`), and chunk score decay pooling.
- **Abstention Calibration (`abstain-probe`)**:
  - Tool to evaluate relevance score distributions on golden queries against out-of-domain negative queries to tune `[rerank] abstain` thresholds (`[DRAG-51]`).
- **Corpus Score Diagnostic (`probe`)**:
  - Tool outputting per-query CSV diagnostics of dense and lexical scores across code and documentation corpora (`[DRAG-25]`).
- **Cold/Warm Latency Profiling (`repeat`)**:
  - Runner measuring sidecar startup time, cold and incremental indexing, context packing, and LLM call durations across fresh process invocations.
- **Chunk Exporter (`export-chunks`)**:
  - JSON-Lines repository chunk exporter for LLM contextual prefix generation experiments (`[DRAG-48]`).
- **Golden Evaluation Query Sets**:
  - Provided golden datasets in `eval/` covering Dendrite Go code, Markdown architecture documentation, mixed code-and-doc questions, and embedag.

### Changed
- **Documentation**: Audited crate README and evaluation results documentation in `eval/RESULTS.md` against test results (`[DRAG-53]`, `[DRAG-59]`).
- **Test Parameterization**: Parameterized unit tests with `rstest = "0.28.0"` (`[DRAG-63]`).
