# Structured topic memory and fidelity guard (planned, DRAG-3)

Topic memories are Markdown documents of emoji-headed sections. `Parser::parse` reads them into a `TopicMemory { topic, sections: [TopicSection { emoji, title, body }] }`, and `Parser::format` writes them back byte-stably.

`FidelityGuard::verify(source, summary) -> FidelityReport` rejects a summary that:

- omits the mandatory disclaimer verbatim,
- drops a key entity present in the source (`missing_key_entities`), or
- introduces terms absent from the source that are on the extrapolation watch list (`extrapolated_terms`).

The report is pass/fail plus the offending items, so callers can regenerate or refuse the summary.
