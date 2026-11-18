# Brief: a reading brief for an agent that is about to explore

`brief` (DRAG-125) answers "where should I start reading?" cheaply. An agent that would otherwise run `rg` over the whole project (many tokens, no ranking) asks for a brief: a short list of files and symbols with line ranges, written by a local model from a retrieval map and a capped grep, so the paid model reads a few hundred tokens instead of every match. It does not answer the question itself; [ask.md](ask.md) does.

1. **Map**: the question's context pack as outlines (`Detail::Map`, 1,500 estimated tokens, at most 12 spans) from the corpora in `corpus` (default `all`); with `paths`, the top 12 hits among those paths instead.
2. **Grep**: `patterns` (regular expressions) when given, else the literal strings in the question: quoted strings and identifiers (`LocateBinary`, `fold_signatures`, `tokens.count`, `a::b`, `DEN-903`). Plain words are left to the map. The matches of all patterns are one search, at most 5 per file and `max_matches` in all (default 60, `[brief] max_matches`), each line cut to 200 characters. An invalid pattern is a `warnings` entry, not a failure.
3. **Prompt**: the numbered spans, the matches and the question; the system prompt asks for `Files to read`, `Key symbols` and `Gaps`, naming only paths from the input, in at most `max_tokens` (default 400, `[brief] max_tokens`).
4. **Model**, always through the LiteLLM gateway:
   - the local model: `local_model`, else `[brief] local_model`, else `[ask] local_model`, else `local-coder`. An empty or canned answer counts as a failure.
   - on failure, `fallback_model` (else `[brief] fallback_model`) when `allow_fallback` is true (the default). **No fallback model is compiled in**: the client chooses it (Dendrite takes the cheapest paid model of its model catalog), so a model that costs money is never used by surprise.
   - when there is no fallback, or it fails too, no model writes the brief: the result is `source_tier: raw`, the ranked spans and the matches as they are, and `local_error` says why.
5. **Citations**: a line of the brief that names a path (a token with a directory and an extension) the input did not contain is dropped, since a model inventing a file sends the agent after something that does not exist. A brief with no line left becomes the raw one, with a warning.

Results are cached in the project's ledger by model and exact prompt, like `ask` (`cached: true`); `fresh: true` asks the model regardless. The result reports the tier, the model that wrote the brief (empty for `raw`), the spans and matches used, the patterns searched, token counts, and `timings` (retrieval stages, grep, model, total).

Settings: `[brief] local_model`, `fallback_model`, `max_tokens`, `max_matches` (`DENDRITE_RAG_BRIEF_LOCAL_MODEL`, `_FALLBACK_MODEL`, `_MAX_TOKENS`, `_MAX_MATCHES`).
