# Walking and chunking

## Walking

- `ignore::WalkBuilder` (parallel), honouring `.gitignore`, `.ignore`, global git excludes and hidden-file rules.
- Always skipped: `.git/`, `target/`, `node_modules/`, `vendor/`, `dist/`, `.dendrite/`, plus every path matched by `Config::exclude` globs.
- Files over `Config::max_file_bytes` (default 1 MiB) are skipped. So is any file whose first 8 KiB contain a NUL byte (binary).
- The language comes from the file extension. Go, Rust, Python, TypeScript/TSX, JavaScript/JSX, Java, C and C++ are parsed with tree-sitter. Markdown is split by headings. Known text formats (configuration, data, SQL, shell, and languages without a bundled grammar) are split as plain text. Files with any other extension are not indexed.

## cAST: split-then-merge over the syntax tree

Fixed-size line windows cut functions in half, and whole-file chunks dilute relevance. cAST (Zhang et al., 2025, *"cAST: Enhancing Code Retrieval-Augmented Generation with Structural Chunking via Abstract Syntax Tree"*) chunks along the syntax tree with a size budget:

1. Parse the file with its tree-sitter grammar.
2. Walk the root's children in order and greedily pack consecutive sibling nodes into the current chunk while its size (non-whitespace characters) stays within `max_chunk_chars` (default 1500).
3. When a single node is larger than the budget, recurse into its children and apply step 2 to them.
4. Merge adjacent undersized chunks (below `min_chunk_chars`, default 200) when the result still fits.
5. A leaf that is still too large (for example a huge string literal) is split by lines.

Gaps between packed nodes (comments, blank lines) stay attached to the following node, so doc comments travel with their function. Files that fail to parse fall back to the text splitter.

**Markdown** is split by heading hierarchy. A section that is too large is split by paragraphs, and every chunk carries its heading path. **Plain text** is split on blank lines, then on lines, within the same budget.

## Definitions, references and scope

Definitions and references come from each grammar's official tag query (`tags.scm`, the same queries GitHub code navigation uses): `@definition.*` captures give the symbols a chunk defines, and `@reference.*` captures give the names it calls or uses. A chunk's **scope** is every definition, plus every `impl` block, whose node fully contains the chunk's non-blank text, outermost first, rendered as `<kind> <name>` (`impl Engine › method search`).

## Chunk record

| Field | Meaning |
|---|---|
| `id` | `blake3(path ‖ 0x00 ‖ start_byte ‖ end_byte ‖ content)`, hex, first 32 chars |
| `content_hash` | `blake3(content)`: the embedding cache key, so identical code is embedded once |
| `path` | Path relative to the project root, `/` separators |
| `lang` | Language tag |
| `start_line`, `end_line` | 1-based, inclusive |
| `scope` | Enclosing named scopes, outermost first, e.g. `impl Engine › fn search` |
| `symbols` | Names defined in the chunk |
| `refs` | Names referenced in the chunk (up to 64), used for the symbol graph |
| `content` | Source text |

## Contextual header

Before indexing, each chunk's text is prefixed with a short header:

```
// path: pkg/sidecar/sidecar.go
// scope: method Locate
<content>
```

This is the no-LLM form of contextual retrieval. Both BM25 and the embedding model see the file and the enclosing scope, which disambiguates generic bodies such as `return nil, err`. The header is used only for indexing; results return the raw content.
