<!-- Imported from Dendrite docs/mcp/dendrite_rag_retrieve.md @ 69ea048 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_rag_retrieve
title: "dendrite_rag_retrieve"
sidebar_label: "dendrite_rag_retrieve"
---

# dendrite_rag_retrieve

Retrieve high-precision AST symbol declarations, complexity metrics, and documentation chunks from the project RAG index for token-efficient prompt synthesis and reasoning

## Input Schema

```json
{
  "properties": {
    "min_score": {
      "description": "Minimum BM25 score threshold (default: 0.01)",
      "type": "number"
    },
    "project_path": {
      "description": "Target project directory to index and search (default: '.')",
      "type": "string"
    },
    "query": {
      "description": "Concept, symbol, or question to search within project RAG index",
      "type": "string"
    },
    "top_k": {
      "description": "Number of context chunks to retrieve (default: 4)",
      "type": "integer"
    }
  },
  "required": [
    "query"
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
      "min_score": 1,
      "project_path": "'.'",
      "query": "sample",
      "top_k": 1
    },
    "name": "dendrite_rag_retrieve"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_rag_retrieve '{"min_score":1,"project_path":"'.'","query":"sample","top_k":1}'
```
