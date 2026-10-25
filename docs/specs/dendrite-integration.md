# Using the sidecar from Dendrite

How Dendrite (the Go control plane, `pkg/sidecar` and `pkg/rag`) runs `dendrite-rag`, what a client has to do with each result, and what is still open on either side. Statements about the Go code were checked against the Dendrite repository on 2026-10-25.

## How Dendrite runs the sidecar

- **One process per call.** `Retriever.run`, `runGraph` and `Rank` start `dendrite-rag serve` (`sidecar.StartWith`), call, and close it. Nothing stays warm between calls: the query-embedding cache, the symbol index and the code graph are rebuilt in each process.
- **Handshake.** The first message is `health`. The client refuses a server whose protocol **major** differs from the one it was built for (`ErrIncompatible`); features added in a minor version are used only when the minor is high enough (`protocolMinor(...) >= AskProtocolMinor`). Minor versions are therefore additive: fields can be added to results (the Go client ignores unknown fields), never removed or retyped.
- **Methods used:** `index` (`full`, `embed_budget_ms`, `focus`), `status` (only when `embed_pending > 0`, to read `embed_coverage`), `search`, `context`, `ask`, `settings` (to read `ask.local_model`), `rank`, `chunks.get` and `graph.*`.
- **Cost of a call.** With the default build and Ollama, opening the engine takes about 20 ms and a no-op `index` of the Dendrite repository (915 files) about 26 ms, 0.1 s wall including the process start (measured 2026-10-25; `open_ms`, `walk_ms` and `duration_ms` in the `index` result). The in-process models (`candle`, `oxillama`, `model2vec`) are loaded when the engine opens; that cost is not measured here and would be paid on every call.

## What a client does with each result

| Field | When it appears | Client action |
|---|---|---|
| `health.features` (1.9) | always | Optional: the cargo features of the binary, to tell which `[embed]` backends and `[storage]` ledgers it can honour before a project is opened. |
| `index.embed_pending`, `embed_error` | embedding stopped (budget) or failed | Coverage is below 100 %. Since 1.9 a failed embedding also counts every chunk as pending (before, `embed_pending` stayed 0 and a client that checks coverage only when `embed_pending > 0`, as Dendrite does, reported 100 %). Lexical and symbol retrieval still work. |
| `index.embed_tokens`, `budget_tokens_remaining` (1.9) | `embed_budget_tokens` or `[embed] budget_tokens` set | Estimated tokens spent and left in this run; call again to continue. |
| `context.abstained`, `ask.abstained` (1.8) | `[rerank] abstain` set and the best reranked hit is below it | The project does not cover the question: tell the person. `ask` returns an empty `answer`, an empty `source_tier` and `model`, and `escalation_needed: false`: do not treat it as a local answer and do not offer the paid model. |
| `ask.escalation_needed` | the local model failed and `allow_paid` was not set | Ask the person, then call again with `allow_paid` or mode `paid`. |
| `status.warnings`, `embed_coverage` | low vector coverage | Show them; dense retrieval is partial. |
| error `data.kind` | `-32000` engine failures | `config` (bad `.dendrite/rag.toml` or settings, or a feature missing from the build), `not_indexed`, `io`, `embed`, `index`, `git`: a `config` error will not clear on retry. |

## Open items

Rust side (this repository):
- Load the embedder and the reranker when first needed instead of in `Engine::open`, so `index`, `status` and `graph.*` do not pay for models they never call. Measure `open_ms` with `candle` first; with the default build there is nothing to gain.
- Report the active ledger, reranker and abstention threshold in `status` (today `settings` returns them, at the cost of a second call).

Go side (Dendrite and `dendrite-mcp-hub`, not changed from here):
- Decode `abstained` in `sidecarAnswer`, `AskResult` and `Retriever.ContextWith`; print a message in `dendrite ask` instead of an empty answer labelled with the default tier. Nothing in `pkg/` or `cmd/` mentions it today, so enabling `[rerank] abstain` in a project would show a blank answer.
- Add `Data` to `sidecar.RPCError`, so `config` errors can be told from transient ones.
- Return `abstained`, `embed_pending`, `embed_error` and `warnings` from the `dendrite_rag_retrieve` MCP tool (`pkg/mcpserver/tools_rag.go` serializes only the chunks).
- Keep one sidecar client per session in long-lived flows (the MCP server) instead of one per call.
- Pass `embed_budget_tokens` when a run must stay under a spend limit (paid embedders).
