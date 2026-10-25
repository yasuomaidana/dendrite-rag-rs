# loom-eval

[![Crate](https://img.shields.io/badge/crate-loom--eval-blue.svg)](Cargo.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](../../Cargo.toml)

`loom-eval` is the evaluation and statistical benchmarking harness for `dendrite-rag-rs`. It evaluates retrieval methods, token economics, latency, and baselines across golden question sets with **bootstrap confidence intervals** and paired comparison tests.

Full evaluation findings and performance results on Apple Silicon are documented in [`eval/RESULTS.md`](../../eval/RESULTS.md).

---

## Evaluation Philosophy

A retrieval engine for AI coding agents cannot simply optimize for top-k document accuracy; it must optimize for **token economics** and **agent efficiency**:
1. **Retrieval Quality**: Does the engine surface the correct file or definition in the top results? (Scored via Hit@k, MRR@10, and nDCG@10).
2. **Token Economics**: How many tokens must an LLM prompt consume before encountering the answer? A method that packs precise 200-token function chunks answers queries at a fraction of the token cost of a tool that reads whole files.
3. **Statistical Significance**: Quality scores include 95% percentile bootstrap confidence intervals (`[low, high]`) to prevent noise-driven regressions. Paired comparisons test whether improvements are statistically distinct from zero.
4. **Baselines**: Evaluates against alternatives:
   - `grep-agent`: How an iterative grep-driven AI agent behaves (ranking files by distinct query words, then total occurrences, and reading whole files).
   - `bm25-file`: BM25 retrieval over whole files.
   - `naive-chunks`: BM25 over fixed 40-line chunk windows.

---

## Metrics Reference

| Metric | Definition |
|---|---|
| **Hit@k** | Fraction of queries where an expected file appears in the top $k$ ranks ($k \in \{1, 5, 10\}$). |
| **MRR@10** | Mean Reciprocal Rank over the top 10 results ($\frac{1}{\text{rank}}$). |
| **nDCG@10** | Normalized Discounted Cumulative Gain with binary relevance (standard CoIR benchmark metric). |
| **recall@B** | Proportion of queries answered within a cumulative prompt budget of $B$ tokens ($B \in \{1\text{k}, 2.5\text{k}, 4\text{k}\}$). |
| **median tokens** | Median number of tokens read across retrieved spans up to and including the first expected file. |
| **both@2.5k** | For questions that require understanding both documentation and implementation, fraction where both appear within 2,500 tokens. |
| **p50 latency** | Median wall-clock retrieval time per query. |

---

## CLI Command Reference

```bash
loom-eval <COMMAND>
```

### 1. `quality`
Runs evaluation of all methods and baselines against golden datasets:

```bash
loom-eval quality <ROOT> <GOLDEN_SETS...> [OPTIONS]
```

**Options:**
- `--only <TEXT>`: Filter to methods matching substring (e.g. `--only bm25`, `--only hybrid`, `--only context`).
- `--json <PATH>`: Write raw per-query evaluation scores to a JSON file for subsequent paired comparisons.
- `--index-dir <DIR>`: Use an alternative index directory.
- `--contexts <JSON>`: Index chunks with generated contextual summaries (`{chunk_id: text}`).

**Example:**
```bash
cargo run -p loom-eval -- quality . eval/golden-dendrite-rag.json
```

**Sample Output:**
```text
### dendrite-rag (25 queries)

| method | hit@1 | hit@5 | hit@10 | MRR@10 | nDCG@10 | recall@1k tok | recall@2.5k tok | recall@4k tok | median tokens to answer | both@2.5k | p50 latency |
|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline grep-agent (whole files) | 0.04 | 0.12 | 0.12 | 0.474 [0.380, 0.580] | 0.542 [0.450, 0.630] | 0.04 | 0.12 | 0.12 | 5007 (96% found) | — | 0.2 ms |
| baseline bm25-file (whole files) | 0.12 | 0.88 | 0.92 | 0.428 [0.333, 0.533] | 0.514 [0.411, 0.616] | 0.04 | 0.16 | 0.16 | 5092 (96% found) | — | 1.0 ms |
| baseline naive-chunks (40 lines, bm25) | 0.36 | 0.84 | 0.92 | 0.548 [0.408, 0.696] | 0.592 [0.453, 0.719] | 0.72 | 0.72 | 0.84 | 1581 (96% found) | — | 1.1 ms |
| search hybrid / code | 0.88 | 0.96 | 0.96 | 0.883 [0.770, 0.960] | 0.840 [0.730, 0.920] | 0.88 | 0.96 | 0.96 | 533 (96% found) | — | 5.2 ms |
| context map / all | 0.76 | 0.92 | 0.92 | 0.768 [0.650, 0.870] | 0.754 [0.640, 0.850] | 1.00 | 1.00 | 1.00 | 57 (96% found) | — | 4.8 ms |
```

### 2. `compare`
Performs paired bootstrap hypothesis testing between a baseline run (A) and candidate run (B):

```bash
loom-eval compare <BASELINE.json> <CANDIDATE.json>
```

Outputs paired difference 95% confidence intervals for MRR@10 and nDCG@10, and reports whether the MRR difference is statistically significant (i.e. 95% CI excludes zero).

### 3. `abstain-probe`
Calibrates the `[rerank] abstain` threshold (off by default) by comparing score distributions on golden queries against unanswerable negative queries:

```bash
loom-eval abstain-probe <ROOT> <GOLDEN_SETS...> --negatives <PATH> --set <NAME> [OPTIONS]
```

**Options:**
- `--negatives <PATH>`: Negative questions JSON file (`eval/golden-negatives.json`).
- `--set <NAME>`: Project key within the negatives file (`dendrite-rag`, `dendrite`, `embedag`).
- `--index-dir <DIR>`: Alternative index directory.

**Example:**
```bash
loom-eval abstain-probe . eval/golden-dendrite-rag.json --negatives eval/golden-negatives.json --set dendrite-rag
```

### 4. `probe`
Emits per-query CSV scores (best dense and lexical scores for code and docs, plus docs score share) across corpora alongside expected query targets:

```bash
loom-eval probe <ROOT> <GOLDEN_SETS...>
```

**Example:**
```bash
loom-eval probe . eval/golden-dendrite-rag.json
```

### 5. `repeat`
Launches fresh sidecar processes to measure cold-start versus warm-cache query timing across stages:

```bash
loom-eval repeat <ROOT> <QUESTION> --bin <PATH> [OPTIONS]
```

**Options:**
- `--bin <PATH>`: Path to the `dendrite-rag` binary.
- `--corpus <TEXT>`: Target corpus (`code` by default, or `docs`, `all`).
- `--llm`: Also call `ask` through the gateway with the configured local model.
- `--runs <N>`: Number of runs to execute (default: 2).

**Example:**
```bash
loom-eval repeat . "how does indexing work?" --bin target/release/dendrite-rag --runs 2
```

### 6. `export-chunks`
Exports all repository chunks to JSON-Lines (`.jsonl`) format for external LLM contextual enrichment:

```bash
loom-eval export-chunks <ROOT> --out <PATH> [OPTIONS]
```

**Options:**
- `--out <PATH>`: Destination JSON-Lines file path.
- `--index-dir <DIR>`: Alternative index directory.

**Example:**
```bash
loom-eval export-chunks . --out chunks.jsonl
```

### 7. `swebench`
File localization on SWE-bench Verified: for each instance the repository is checked out at the instance's `base_commit`, indexed incrementally (the Merkle diff re-chunks only the files that changed since the previous commit of that repository), and the problem statement is the query. A ranked file is correct only if it is exactly a file the gold patch modifies (pre-image path; files the patch creates are not counted).

Prepare the data once (the dataset file is git-ignored; `--clone-repos` makes full clones, a few GB for the 12 repositories):

```bash
uv run --no-project --with pyarrow python -I eval/swebench/import.py eval/swebench/verified.json \
    --repos-cache /tmp/swebench-repos --clone-repos
```

```bash
loom-eval swebench <INSTANCES_JSON> --repos-cache <DIR> --index-root <DIR> [OPTIONS]
```

**Options:**
- `--repos-cache <DIR>`: Directory holding the cloned repositories (`<owner>/<name>`); an instance whose repository is missing, or whose checkout fails, is counted as skipped and reported.
- `--index-root <DIR>`: Directory where repository indexes are stored.
- `--stratified`: Run only the Stratified-120 subset.
- `--repo <NAME>`: Filter by repository name substring.
- `--limit <N>`: Limit number of instances.
- `--smoke`: Run a 5-instance smoke test.
- `--json <PATH>`: Write the results in the `loom-eval compare` format (sets `swebench-verified`, one per repository and one per leakage class), plus `<PATH>.detail.json` with the per-instance scores.

The embedder is the project's (`DENDRITE_RAG_EMBED_BACKEND=none` gives lexical-only runs, where `hybrid` is BM25 plus symbol names). Methods: `grep-agent`, `bm25-file` (whole files), `chunk-bm25` and `hybrid` with chunk scores pooled to files by the maximum, the `-decay` variants (`s0 + 0.5 s1 + 0.25 s2` of the top three chunks), and `chunk-bm25-notests` (test files and numbered migrations removed). Columns are Hit@k (a gold file within the top k), MRR@10 and, for multi-file instances, All-gold@10; the table is repeated per leakage class (`path_explicit`: the statement names a gold file, `traceback_symbol`, `opaque_nl`) and macro-averaged over repositories.

**Example:**
```bash
DENDRITE_RAG_EMBED_BACKEND=none loom-eval swebench eval/swebench/verified.json \
    --repos-cache /tmp/swebench-repos --index-root /tmp/swebench-idx --stratified --json run.json
loom-eval compare run-a.json run-b.json
```

---

## Golden Query Sets and External Benchmarks

Golden evaluation sets are stored in [`eval/`](../../eval):

- `golden-dendrite-rag.json`: 25 natural-language queries testing the layout and components of this repository.
- `golden-dendrite.json`: 37 natural-language code queries for the Dendrite Go codebase.
- `golden-dendrite-docs.json`: 27 documentation questions answered exclusively by Markdown architecture specs.
- `golden-dendrite-mixed.json`: 12 multi-hop queries requiring both architectural documentation and code implementations.
- `golden-embedag.json`: 25 queries testing Merkle-tree change detection and hash indexing.
- `golden-negatives.json`: Questions out-of-domain for calibration of abstention thresholds (8 questions per repository: `dendrite-rag`, `dendrite`, `embedag`).

### External Benchmark Scripts

- `eval/swebench/import.py`: Imports SWE-bench Verified from Hugging Face (`princeton-nlp/SWE-bench_Verified`) via pyarrow, extracting gold files from patches, generating deterministic Q1/Q2/Q3 query variants, tagging leakage classes, and marking the Stratified-120 subset.
- `eval/coir/import.py`: Materializes CoIR retrieval datasets (e.g. CosQA: 20,604 Python functions, 500 test queries) as corpus directories and golden query sets.
- `eval/contextual/gen.py`: Generates contextual chunk prefixes with a model via `agy` (DRAG-48).

---

## Examples Guide

The crate provides a runnable example in `examples/`:

### `quick_eval`
Demonstrates how to evaluate queries, compute token-budgeted recall, and calculate bootstrap confidence intervals programmatically:

```bash
cargo run --example quick_eval -p loom-eval
```

**Sample Output:**
```text
=== Loom-Eval Metrics & Confidence Interval Demo ===

### Synthetic Golden Set (3 queries)

| method | hit@1 | hit@5 | hit@10 | MRR@10 | nDCG@10 | recall@1k tok | recall@2.5k tok | recall@4k tok | median tokens to answer | both@2.5k | p50 latency |
|---|---|---|---|---|---|---|---|---|---|---|---|
| method: bm25   | 0.33 | 0.67 | 0.67 | 0.500 [0.000, 1.000] | 0.544 [0.000, 1.000] | 0.67 | 0.67 | 0.67 | 550 (67% found) | — | 3.0 ms |
| method: hybrid | 1.00 | 1.00 | 1.00 | 1.000 [1.000, 1.000] | 1.000 [1.000, 1.000] | 1.00 | 1.00 | 1.00 | 300 (100% found) | — | 6.0 ms |
```

---

## Testing

Run unit tests for `loom-eval`:

```bash
cargo test -p loom-eval
```
