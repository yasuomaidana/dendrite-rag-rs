# Git history search

The question "why did this code change?" is answered from the repository's history: commit messages next to the diffs that implement them.

## Documents

One document per (commit, changed file) holds:

- the commit message (subject and body),
- the file path,
- that file's diff, with 1 line of context, truncated to 2,000 characters.

Commits without file changes, or touching more than 50 files (mass renames, vendoring), get a single message-only document. Merge commits are skipped (`--no-merges`). Their changes are already in the merged commits.

Fields use the same code tokenizer as the code index. BM25 boosts are: message 2.0, path 1.5, diff 1.0.

## Indexing

- History is read by streaming `git log -p` through the `git` CLI. dendrite-rag-rs does not reimplement Git object access, which belongs to dendrite-git-rs.
- The index lives in `<index_dir>/history/`, and the newest indexed commit is `history_head` in the ledger.
- Incremental: later runs read only `history_head..HEAD`. When `history_head` is no longer an ancestor of `HEAD` (after a rebase or a reset), the history index is rebuilt.
- The first run reads at most `max_commits` (default 2,000) commits, newest first.
- `history.search` updates incrementally before searching. When `HEAD` has not moved, the update is one `git rev-parse`.

Measured on the Dendrite repository (201 commits): first search including indexing 1.5 s, later searches 0.06 s.

## Interface

- RPC: `history.index {root, max_commits?}` and `history.search {root, query, top_k?}`; see [protocol.md](protocol.md).
- CLI: `dendrite-rag history <root> <query> [-k N]`.
- Failures (not a repository, `git` missing) are reported with `data.kind = "git"`.
- A repository without commits gives an empty result.
