<!-- Imported from Dendrite docs/mcp/dendrite_store_rm.md @ 01e70c1 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_store_rm
title: "dendrite_store_rm"
sidebar_label: "dendrite_store_rm"
---

# dendrite_store_rm

Safely removes SQLite analysis records and unlinks compressed blobs with confirmation gate

## Input Schema

```json
{
  "properties": {
    "force": {
      "description": "Mandatory confirmation gate to perform permanent deletion",
      "type": "boolean"
    },
    "store_id": {
      "description": "Project / Store identifier to delete",
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
      "force": false,
      "store_id": "sample"
    },
    "name": "dendrite_store_rm"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_store_rm '{"force":false,"store_id":"sample"}'
```
