<!-- Imported from Dendrite docs/mcp/dendrite_rag_ask.md @ 69ea048 on 2026-10-18. Source of truth for this repo's scope; keep in sync with the Dendrite copy until the extraction umbrella lands. -->

---
id: dendrite_rag_ask
title: "dendrite_rag_ask"
sidebar_label: "dendrite_rag_ask"
---

# dendrite_rag_ask

Ask architectural, API, or codebase questions with local RAG retrieval and optional paid cloud LLM fallback

## Input Schema

```json
{
  "properties": {
    "auto_approve_paid": {
      "description": "Automatically escalate to paid cloud model if local LLM fails without prompt",
      "type": "boolean"
    },
    "local_model": {
      "description": "Preferred local model",
      "type": "string"
    },
    "paid_model": {
      "description": "Paid cloud model fallback (default: 'gemini-2.5-pro')",
      "type": "string"
    },
    "project_path": {
      "description": "Target project directory to index and search (default: '.')",
      "type": "string"
    },
    "question": {
      "description": "Question to ask about the project",
      "type": "string"
    },
    "top_k": {
      "description": "Number of context chunks to retrieve (default: 4)",
      "type": "integer"
    }
  },
  "required": [
    "question"
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
      "auto_approve_paid": false,
      "local_model": "sample",
      "paid_model": "'gemini-2.5-pro'",
      "project_path": "'.'",
      "question": "sample",
      "top_k": 1
    },
    "name": "dendrite_rag_ask"
  }
}
```

### CLI

```bash
dendrite mcp call dendrite_rag_ask '{"auto_approve_paid":false,"local_model":"sample","paid_model":"'gemini-2.5-pro'","project_path":"'.'","question":"sample","top_k":1}'
```
