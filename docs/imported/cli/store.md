<!-- Imported from Dendrite docs/cli/store.md @ 01e70c1 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: store
title: "dendrite store"
sidebar_label: "store"
---

# dendrite store

Persistent segmented store, memory checkpointer, and chunk query

## Category

Soma & Axon Hillock

## Aliases

- `engram-store`
- `engram`
- `gram`

## Subcommands

| Subcommand | Description |
| :--- | :--- |
| `status` | Inspect persistent segmented store status and chunk count |
| `chunks` | List stored memory chunks and vector embeddings |
| `query` | Semantic similarity query over stored code memory |
| `commit` | Checkpoint active workspace state into persistent store |

## Flags

None

## Examples

```bash
dendrite store status
dendrite store chunks
```
