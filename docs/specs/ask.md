# Ask: retrieval-augmented generation

`ask` (DRAG-38) answers a question from the project. It moved here from Dendrite's Go `pkg/rag/ask.go`, whose prompts and answer handling it keeps, so answers do not change with the move; Dendrite's `dendrite ask` is now a client of it.

1. **Context**: the context pack for the question (2,500 estimated tokens), or the top `top_k` hits, from the corpora in `corpus` (default `all`; [retrieval.md](retrieval.md) § Corpora).
2. **Prompt**: numbered sources `[n] Source: <path> (lines a-b)` with their heading (scope) and text, then the question.
3. **Model**, by `mode`, always through the LiteLLM gateway (`LITELLM_PROXY_URL`, `LITELLM_MASTER_KEY`):
   - `local` (default): the local model (`local_model`, else `[ask] local_model`, else `local-coder`). An empty answer, or an engine's canned placeholder, counts as a failure.
   - `hybrid`: a local draft, then the paid model (`paid_model`, else `[ask] paid_model`, else `gemini-2.5-pro`) verifies and refines it; when refinement fails the draft is returned.
   - `paid`: the paid model directly.
4. **Escalation**: when the local model fails, the paid model is used only if `allow_paid` is set. Otherwise the result has `escalation_needed: true` and no answer: the decision to pay belongs to the person, so the client asks and calls again (`allow_paid` or mode `paid`).
5. **Commands**: an answer that is canned or too short is replaced by the `dendrite …` commands the context documents, and a question asking for commands gets them appended when the answer names none.

Answers are cached in the project's ledger by model and exact prompt: the same question about unchanged code returns without a model call (`cached: true`); a changed file changes the retrieved context, hence the prompt. `fresh: true` asks the model regardless.

The result reports the tier (`local-rag`, `hybrid`, `paid-cloud`), the model, the local draft (hybrid), token counts, the hits it used, and `timings`: the retrieval stages, the time in local and paid model calls, and the total, in milliseconds.

Settings: `[ask] local_model`, `paid_model` (`DENDRITE_RAG_ASK_LOCAL_MODEL`, `DENDRITE_RAG_ASK_PAID_MODEL`).
