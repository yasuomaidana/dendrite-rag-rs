---
name: dendrite-workflow
description: Dendrite-first workflow for every model (Claude, Antigravity/AGY, Gemini, Copilot). Use before starting a session, creating a branch, clone or worktree, committing, or delegating work to another model.
---

# Dendrite-First Workflow

> Managed by `dendrite soma prepare`; edits here are overwritten. Change `pkg/env/bootstrap/workflow.go` instead.

Every agent session in this repository runs through Dendrite. Commands use Neuro-Lore names; the plain names (`env`, `plane`, `agent`, `commit`, ...) still work as aliases, and `dendrite <command> --help` shows every flag.

1. Start every session with `dendrite session resume`; it also removes merged, clean working copies and prints the Engram memory snapshot when Engram is installed (`dendrite soma install engram`; Engram holds agent memory, rag-rs holds code search).
2. Pick work from Plane with `dendrite chronos next` (alias `plane next`, or the `dendrite_ticket_smart_select` MCP tool), never from `DEV_STATUS.md`.
3. Every change follows one ticket through its lifecycle, and a write lock enforces it: `dendrite hook pre-edit` refuses file changes in a working copy that has no claim. Pull the ticket (`dendrite chronos next`) or create it (`dendrite chronos create --name "..." --desc "goal and acceptance criteria"`), design the plan, then start it with `dendrite work new <ticket>`, which moves it to In Progress and claims it for the new copy (a claim by hand: `dendrite chronos claim <ticket>` or the `dendrite_ticket_claim` MCP tool; `dendrite_query_context` returns the ticket, its criteria and any saved plan). Each `dendrite quantal` commit and each `dendrite session checkpoint` leaves a comment on the ticket, and a plan you did not finish is stored on it (`dendrite chronos plan push`) so the next session resumes from it. When the work is finished run `dendrite chronos finish <ticket>` (MCP `dendrite_ticket_finish`): it runs the quality gate and checks the diff against each `[AC-n]`, then sets `done` when the project allows it (directive `[done] auto_done`) and the ticket has no `needs-human-review` label, `to verify` otherwise, and `blocked` with a handoff (completed, challenges, missing) when a check fails. To stop for another reason use `dendrite chronos handoff <ticket>` (blocked: write what it waits on and what is missing), `dendrite chronos ticket set <ticket> cancelled` when it is dropped or `backlog` when it is parked. Record every question you asked and the answer you got on the ticket: Claude Code's `AskUserQuestion` is stored by a hook, any other agent runs `dendrite chronos decision <ticket> --question "..." --answer "..."` (MCP `dendrite_ticket_decision`), and `dendrite chronos comments <ticket>` reads them and any handoff back. The limits every change is held to (production lines per file, coverage, duplication, whether agents may set `done`) are the project's directives in `.github/dendrite/directives.toml`; repeated code is removed with a helper function, or with a macro where a function cannot express it, and Rust tests use the `rstest` crate (`rstest = "0.28.0"`: `#[case]`, `#[values]`, `#[fixture]`) rather than hand-written copies or imitating helpers (directive `[duplication] strategies`); change them with `dendrite directive "<rule>"`, never by hand-editing CI, and check a change against them with `dendrite allostasis gate` (`--since origin/main` judges only what you changed). A challenge found mid-plan is a new ticket linked with `dendrite chronos link`. Bypass the guards only deliberately, with `--no-ticket-guard`.
4. Every repository we can modify is a Plane project: run `dendrite chronos project ensure` in it (it writes the local `.dendrite/plane.json` that `dendrite chronos`, `sync` and agents read; never commit anything under `.dendrite/`, and in a fresh clone this command recovers the project's identity). Repositories that depend on each other belong to the same synapse (`dendrite synapse create`). `dendrite chronos project audit` lists any repository that breaks this rule.
5. Work on your own branch, never directly on `main`, `main` or `master` (nor on the trunk of another ticket): run `dendrite work new <ticket>` for each ticket. It creates a git worktree in `dendrite-work/` next to the main checkout, on a branch named after the ticket: its own trunk `<TICKET>-<slug>/main`, which merges into `main`, or, for a sub-ticket of a Plane parent, `<ROOT>-<slug>/<TICKET>-<slug>`, which merges into the trunk `<ROOT>-<slug>/main` of its top-most parent (`dendrite branch show --ticket <ticket>` says which; `dendrite branch parent <ticket> <parent>` links tickets; `--target <branch>` overrides). Never hand-make `git clone` or `git worktree add ../<dir>` copies: those mix with human projects and are never cleaned up.
6. Commit with `dendrite quantal` (alias `commit`; it references the ticket and pushes your branch to origin, never to main/master).
7. When a sub-ticket is finished, merge its branch into its parent's trunk (`dendrite branch show` names it) and push, so the feature's work syncs through it: from your copy run `git fetch origin && git merge origin/<trunk>`, fix conflicts and re-run the tests on your branch, then `git push origin HEAD:<trunk>`. If the push is rejected, someone else merged first: fetch, merge and push again. A trunk reaches `main` only by pull request: `dendrite chronos finish` on the ticket that owns the trunk (a ticket with no parent, or the parent once every sub-ticket is done) pushes it and opens the pull request, the ticket waits in To Verify, and a person merges it. Never merge into or push `main`/`master`.
8. Explore with Dendrite before any raw text search, cheapest first: `dendrite rag search`/`dendrite_rag_retrieve` (detail `map`, then `dendrite rag show`/`dendrite_rag_show` for the bodies you need), `dendrite rag symbols` for definitions and references, `dendrite graph` (`dendrite_graph_explain`, `dendrite_graph_path`) for callers and structure, `dendrite rag history` for past changes, and `dendrite ask` for questions. `dendrite rag grep` is the exact-pattern fallback (index-aware, capped). Use `rg`/`grep`/`find` only when those return nothing, and then scoped to a path with a result cap, because unscoped `rg` output costs far more tokens than a retrieval map.
9. Questions about tickets (status, blockers, what a ticket is for) go to Plane through Dendrite: `dendrite ask --corpus tickets "..."` or `dendrite rag search --corpus tickets "..."` (MCP: `dendrite_rag_retrieve` with corpus `tickets`) for open questions, and `dendrite chronos`, `dendrite_plane_issues` or `dendrite_ticket_smart_select` for exact lookups. Never use `DEV_STATUS.md` or a search of the repository for this.
10. Route every LLM call through Dendrite: the LiteLLM gateway (`http://localhost:4000`) for model access and `dendrite bouton jobs submit --isolate` (alias `agent jobs`) to delegate to Antigravity (AGY) or other models. Do not call provider APIs or spawn other agent CLIs directly.
11. Run `dendrite session checkpoint` after each finished ticket and before long waits.
12. When a copy is finished and merged, `dendrite work gc` (or the next resume/checkpoint) deletes it; `dendrite work done <name>` removes one explicitly.

## Projects and synapses

If you can modify a repository's code, it is a project: it has its own Plane project, and its `.dendrite/plane.json` (local and untracked, like all of `.dendrite/`, together with `.dendrite/project.id`) tells every Dendrite command which board to use. If it depends on or is used by another editable repository, both are members of one synapse.

```bash
dendrite chronos project ensure [dir] [--identifier ID] [--import DEV_STATUS.md] # adopt or create, canonical states, link
dendrite chronos project show [dir]                   # which Plane project this repository uses
dendrite chronos project audit [--roots dir,...]      # editable repos without a project, related repos without a shared synapse
dendrite synapse create <id> --members <dir>=alias:role,...   # link related repositories
```

A repository whose origin belongs to someone else (a third-party clone) is not editable and needs no project.

## Working copies

```bash
dendrite work new DEN-123          # ../dendrite-work/<repo>-DEN-123-<slug> on branch DEN-123-<slug>/main
dendrite work list                 # status: dirty, unpushed, merged, upstream gone
dendrite work done DEN-123         # remove one copy (refuses unsaved work without --force)
dendrite work gc [--dry-run]       # remove merged + clean copies
dendrite work adopt [--dry-run]    # move legacy sibling worktrees into dendrite-work/
dendrite work dispose [--dry-run]  # ask to delete what merged work left: repair copies and branches, finished features
```

A copy is deleted automatically only when its branch is merged into main/master/main or the target recorded for it, e.g. the trunk of its parent ticket (or its upstream branch was deleted) **and** it has no uncommitted or unpushed work. Copies used by in-flight agent jobs, the current directory, and story-fork variants are never removed.

## Finishing a ticket

Each agent works on its own branch. A sub-ticket merges into the trunk `<ROOT>-<slug>/main` of its top-most parent when it is done (`dendrite branch show --ticket DEN-123` says which). From your `dendrite-work/` copy:

```bash
dendrite quantal --ticket DEN-123        # commit and push the ticket's branch
dendrite chronos finish DEN-123          # the quality gate and the acceptance review
git fetch origin
git merge origin/<parent trunk>          # take in the other sub-tickets' work; fix conflicts, re-run the tests
git push origin HEAD:<parent trunk>      # fast-forward the trunk to your branch
dendrite session checkpoint              # record it; the merged copy is cleaned up later
```

A rejected push means the trunk moved since your fetch: fetch, merge and push again. A ticket with no parent, and the parent of a feature whose sub-tickets are all done, works on its own trunk `<TICKET>-<slug>/main`: there `dendrite chronos finish` pushes the trunk and opens a pull request into `main`, the ticket waits in To Verify, and a person merges it. Never merge into or push `main`/`master`.

## Delegating to other models

```bash
dendrite bouton jobs submit --ticket DEN-123 --agent agy --scope pkg/foo --prompt-file prompt.md --isolate
dendrite bouton jobs wait --ticket DEN-123   # block until the job finishes
```

`--isolate` runs the job in the ticket's `dendrite-work/` copy, so the delegated model never edits the orchestrator's checkout.

### What to delegate

Delegate work that is mechanical and checkable: updating test expectations after a deliberate behavior change, fixtures and golden files, docs and seed or config edits, boilerplate from a clear spec, first drafts of modules whose protocol you already fixed. Use `--agent agy` (complexity 1-3 picks a fast model) or a local Ollama model for the cheapest tier. Keep design, protocols, concurrency, ranking and debugging with the strong model, and review everything a delegate returns.

### How to delegate well

- Give the job its own ticket's copy (`--ticket`, `--worktree` or `--isolate`) and a tight `--scope` of the paths it may change; never edit those files yourself while it runs.
- Write the acceptance in the prompt: what must pass (`go test ./pkg/x`), what must not be weakened (no loosened assertions), and that it must not commit.
- `dendrite bouton jobs wait --ticket DEN-123`, then read the diff (`git diff`), re-run the tests yourself, and only then commit with `dendrite quantal`.
- Tests use real local servers and binaries. The zero-mock gate rejects `fake*`/`mock*`/`stub*` identifiers and `t.Skip`; an opt-in suite goes behind a build tag instead.

## Ending the plan

When the plan ends, record where the ticket stands before moving on: `dendrite chronos ticket set DEN-123 "to verify"` when it is finished, `blocked` when it waits on something (say what in the ticket), `cancelled` when it is dropped, `backlog` when it is parked. Agents never set `done`; a human signs off from To Verify.
