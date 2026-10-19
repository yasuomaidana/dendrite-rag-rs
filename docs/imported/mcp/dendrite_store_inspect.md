<!-- Imported from Dendrite docs/mcp/dendrite_store_inspect.md @ 01e70c1 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_store_inspect
title: "dendrite_store_inspect"
sidebar_label: "dendrite_store_inspect"
---

# dendrite_store_inspect

Detailed chunk-by-chunk and segment analysis breakdown for a specific stored project

## Input Schema

```json
{
  "properties": {
    "store_id": {
      "description": "Project / Store identifier",
      "type": "string"
    }
  },
  "required": [
    "store_id"
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
      "store_id": "sample"
    },
    "name": "dendrite_store_inspect"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_store_inspect '{"store_id":"sample"}'
```
