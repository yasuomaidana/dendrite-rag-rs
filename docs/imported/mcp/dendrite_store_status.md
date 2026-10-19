<!-- Imported from Dendrite docs/mcp/dendrite_store_status.md @ 01e70c1 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_store_status
title: "dendrite_store_status"
sidebar_label: "dendrite_store_status"
---

# dendrite_store_status

Query real-time storage status, segments count, and checkpoint states of the persistent segmented analysis store

## Input Schema

```json
{
  "properties": {
    "project": {
      "description": "Optional project identifier filter",
      "type": "string"
    }
  },
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
    "name": "dendrite_store_status"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_store_status '{"project":"sample"}'
```
