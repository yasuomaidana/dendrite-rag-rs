<!-- Imported from Dendrite docs/mcp/dendrite_store_query_partial.md @ 01e70c1 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_store_query_partial
title: "dendrite_store_query_partial"
sidebar_label: "dendrite_store_query_partial"
---

# dendrite_store_query_partial

Query committed segments for a project, including projects currently in ANALYZING_PARTIAL state

## Input Schema

```json
{
  "properties": {
    "project": {
      "description": "Project identifier",
      "type": "string"
    }
  },
  "required": [
    "project"
  ],
  "type": "object"
}
```

## Example Call

### JSON-RPC

```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "arguments": {
      "project": "sample"
    },
    "name": "dendrite_store_query_partial"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_store_query_partial '{"project":"sample"}'
```
