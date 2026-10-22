# Embedders: choosing a backend and a model

Dense retrieval needs an embedding model. The project owner can choose any backend and model; every backend is a first-class option. Without a choice, `backend = "auto"` takes the first available GPU-capable backend, Rust first (see [Auto](#auto-gpu-first)). Lexical and symbol retrieval work without any model (`backend = "none"`).

## Where the choice lives

1. `<project>/.dendrite/rag.toml`, committed with the project so every clone and agent uses the same model (`DENDRITE_RAG_CONFIG` points to another file).
2. The environment, which overrides the file for one machine or one run.

```toml
[embed]
backend = "ollama"              # auto (default) | openai | ollama | hf | candle | oxillama | model2vec | none
model = "nomic-embed-text"      # optional: each backend has a default
url = "http://localhost:11434"  # optional, HTTP backends only
# api_key_env = "HF_TOKEN"      # the variable holding the bearer key (keys never go in the file)
# document_prefix = "search_document: "
# query_prefix = "search_query: "
# pooling = "mean"              # mean | cls | last (in-process and token-level outputs)
# batch = 32                    # texts per request or forward pass

[rerank]
# backend = "gateway"           # gateway (default) | candle | none
# model = "bge-reranker-v2-m3"  # cross-encoder; off when unset on gateway
# url = "http://localhost:4000" # gateway URL
# batch = 16                    # in-process batch size

[index]
# max_file_bytes = 1048576

[graph]
# resolution = 1.0

[graph.llm]
# enabled = false
# model = "local-qwen3-8b-q4_k_m"   # any model the LiteLLM gateway serves; required when enabled
# summaries = false
# concepts = false
```

| Variable | Overrides |
|---|---|
| `DENDRITE_RAG_EMBED_BACKEND` | `embed.backend`. A backend different from the file's drops the file's `model`, `url`, prefixes and pooling, because they belong to the file's backend |
| `DENDRITE_RAG_EMBED_MODEL`, `_URL`, `_API_KEY_ENV`, `_DOCUMENT_PREFIX`, `_QUERY_PREFIX`, `_POOLING`, `_BATCH` | the matching `embed.*` key |
| `DENDRITE_RAG_RERANK_BACKEND`, `_MODEL`, `_URL` | `rerank.backend`, `rerank.model`, `rerank.url`. A backend different from the file's drops the file's model and URL |
| `DENDRITE_RAG_MAX_FILE_BYTES` | `index.max_file_bytes` |
| `DENDRITE_RAG_GRAPH_RESOLUTION` | `graph.resolution` |
| `DENDRITE_RAG_GRAPH_LLM_ENABLED`, `_MODEL`, `_SUMMARIES`, `_CONCEPTS` | `graph.llm.enabled`, `graph.llm.model`, `graph.llm.summaries`, `graph.llm.concepts` |
| `DENDRITE_RAG_ASK_LOCAL_MODEL`, `DENDRITE_RAG_ASK_PAID_MODEL`, `DENDRITE_RAG_ASK_DOCS_SHARE` | `ask.local_model`, `ask.paid_model`, `ask.docs_share` ([ask.md](ask.md)) |
| `DENDRITE_RAG_STORAGE_MODE`, `DENDRITE_RAG_STORAGE_REPO` | `storage.mode` (`local`, `remote`, `edge`), `storage.repo` |
| `DENDRITE_RAG_PG_URL` | the shared tier's URL; environment only ([store.md](store.md) § PostgreSQL) |

Unknown keys, unknown backends and malformed values are errors, never silently replaced by a default.

`dendrite-rag embedders` lists the backends this build supports, with their default models. `dendrite-rag settings <root>` prints the settings in effect for a project. The `embedders` and `settings` RPC methods return the same data.

## Auto: GPU first

`auto` (the default) resolves once per process, in this order, and logs its choice on stderr (`dendrite-rag: embedder auto -> candle (candle on metal)`):

1. `candle` when the binary was built with `candle-metal` or `candle-cuda` and the GPU opens (default model `jinaai/jina-embeddings-v2-base-code`).
2. `hf` when `HF_TEI_ENDPOINT` names a Text Embeddings Inference server (Rust, GPU).
3. `ollama` when its server answers within 0.5 s.
4. `candle` on the CPU when the binary has the `candle` feature.
5. `none`: lexical and symbol retrieval only.

A configured `model`, prefixes, pooling and batch carry over to the chosen backend. oxillama is never chosen automatically: oxillama-runtime 0.1.4 offloads only Q4_0 single-token decode to the GPU, so its embeddings run on the CPU (over 3 s per chunk measured). Because the choice can differ between machines, a project that needs identical vectors everywhere pins `backend` and `model`.

Recommended builds: `cargo install --locked --git https://github.com/yasuomaidana/dendrite-rag-rs --features candle-metal` on Apple Silicon, `--features candle-cuda` on NVIDIA (needs the CUDA toolkit), none to use Ollama.

## Backends

| Backend | Runs | Wire / runtime | Default model | Key |
|---|---|---|---|---|
| `openai` (aliases `gateway`, `litellm`) | any OpenAI-compatible server: the LiteLLM gateway (default `LITELLM_PROXY_URL`, then `http://localhost:4000`), vLLM, LM Studio, `oxillama serve` | `POST {url}/v1/embeddings` | `local-nomic-embed-text-latest` | `LITELLM_API_KEY`, `LITELLM_MASTER_KEY`, `OPENAI_API_KEY` |
| `ollama` | an Ollama server (default `OLLAMA_HOST`, then `http://localhost:11434`) | `POST {url}/api/embed` with `truncate: true` | `nomic-embed-text` | — |
| `hf` (aliases `huggingface`, `tei`) | Hugging Face Inference: serverless (no `url`), a dedicated endpoint (`*.endpoints.huggingface.cloud`), or any Text Embeddings Inference server (`{url}/embed`) | `POST` `{"inputs": [...], "truncate": true}`. Sentence vectors are used as returned; token-level outputs are pooled | `BAAI/bge-base-en-v1.5` | `HF_TOKEN`, `HUGGING_FACE_HUB_TOKEN`, then the `huggingface-cli login` token file. Serverless without a token is an error |
| `candle` | in process, build feature `candle` (`candle-metal` for the Apple GPU, `candle-cuda` for NVIDIA, `candle-accelerate` for Accelerate BLAS on the CPU) | safetensors encoders on candle: BERT family (bge, e5, MiniLM, RoBERTa), NomicBERT (nomic-embed-text v1/v1.5), JinaBERT (jina-embeddings-v2), and the query/key post-norm JinaBERT of the code model `jinaai/jina-embeddings-v2-base-code`, which candle-transformers lacks and dendrite-rag implements in `embed::jina_code` (vectors match the reference PyTorch model at cosine 1.0000). The architecture is read from `config.json`; others are rejected with a pointer to the server backends. Inputs are cut at 512 tokens; batches of 16 grouped by length | `jinaai/jina-embeddings-v2-base-code` | `HF_TOKEN` only for gated repos |
| `oxillama` | in process, build feature `oxillama` (pure Rust, no C/C++; needs Rust 1.89). **CPU only for embeddings**: oxillama-runtime 0.1.4 runs multi-token prefill on the CPU | GGUF decoder embedders on OxiLLaMa (`oxillama-runtime`): Qwen3-Embedding and other Llama, Qwen3, Gemma, Mistral or Phi based embedders. The model is a `.gguf` path, a hub repository (its `Q8_0` file is preferred, then `f16`), or `<repo>/<file>.gguf`; the tokenizer comes from the GGUF metadata. One input per forward pass | `Qwen/Qwen3-Embedding-0.6B-GGUF` | `HF_TOKEN` only for gated repos |
| `model2vec` | in process, build feature `model2vec` | static token embeddings (model2vec-rs) | `minishlab/potion-code-16M-v2` | — |
| `none` | — | lexical and symbol rankings only | — | — |

Server backends send batches of 32 texts. Inputs over 8,000 bytes are cut at a character boundary.

## Model conventions applied automatically

Asymmetric models were trained with task prefixes. They are applied by model family unless `document_prefix` or `query_prefix` is set:

| Family (name contains) | Document prefix | Query prefix | Pooling |
|---|---|---|---|
| `nomic-embed` | `search_document: ` | `search_query: ` | mean |
| `qwen3-embedding` | — | `Instruct: Given a code search query, retrieve relevant code and documentation\nQuery: ` | last token |
| `e5-` | `passage: ` | `query: ` | mean |
| `bge-…-en`, `mxbai-embed`, `arctic-embed` | — | `Represent this sentence for searching relevant passages: ` | CLS |
| anything else | — | — | mean |

## Changing the model

Vectors are only comparable within one model. The ledger records the model id of the stored vectors (`openai` uses the bare model name, the other backends `<backend>:<model>`). When it differs from the configured one, the vector index is rebuilt at open. The embedding cache is keyed by `(model id, content hash)`, so switching back to a previous model reuses its cached vectors instead of calling the model again.

## Choosing

Measured on 30 golden questions (`eval/RESULTS.md`):

- **Best quality, in process:** `candle` (Metal or CUDA build) with `jinaai/jina-embeddings-v2-base-code`, the default of `auto` on a GPU build: hybrid MRR 0.807, the right file in the top 10 for every question.
- **A server already running:** `ollama` (or `openai` through the gateway) with `nomic-embed-text`: hybrid MRR 0.770.
- **Shared GPU box or managed service:** `hf` against a TEI server or a dedicated endpoint.
- **CPU-only machines:** `candle` on the CPU gives the same quality but took 46 minutes for a 5,300-chunk repository; Ollama took 8.4 minutes. `oxillama` (Qwen3-Embedding from one GGUF file) runs over 3 s per chunk and suits small projects or query-only use.
- **Fully offline, nothing else installed:** `model2vec`, which embeds a repository in seconds at lower quality.
- **No model at all:** `none`. Lexical and symbol retrieval carry most identifier-shaped queries.
