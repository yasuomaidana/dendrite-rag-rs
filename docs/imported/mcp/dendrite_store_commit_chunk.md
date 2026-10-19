<!-- Imported from Dendrite docs/mcp/dendrite_store_commit_chunk.md @ 01e70c1 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_store_commit_chunk
title: "dendrite_store_commit_chunk"
sidebar_label: "dendrite_store_commit_chunk"
---

# dendrite_store_commit_chunk

Atomically commit a chunk of analyzed segments and compressed blobs to the persistent store

## Input Schema

```json
{
  "properties": {
    "chunk_id": {
      "description": "Unique chunk identifier",
      "type": "string"
    },
    "data": {
      "description": "Raw text/code payload for the segment",
      "type": "string"
    },
    "identifier": {
      "description": "Symbol, function, or file identifier",
      "type": "string"
    },
    "is_final": {
      "description": "Set true if this is the final completed chunk (StateStored)",
      "type": "boolean"
    },
    "project": {
      "description": "Project name or ID",
      "type": "string"
    },
    "type": {
      "description": "Segment type",
      "enum": [
        "file",
        "function",
        "cfg_block",
        "ast_graph"
      ],
      "type": "string"
    }
  },
  "required": [
    "project",
    "chunk_id",
    "identifier",
    "data"
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
      "chunk_id": "sample",
      "data": "sample",
      "identifier": "sample",
      "is_final": false,
      "project": "sample",
      "type": "sample"
    },
    "name": "dendrite_store_commit_chunk"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_store_commit_chunk '{"chunk_id":"sample","data":"sample","identifier":"sample","is_final":false,"project":"sample","type":"sample"}'
```
