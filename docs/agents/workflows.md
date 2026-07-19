# Workflows

When and how to use EngramDB's tools as an AI coding agent. The core loop: **query before doing project-specific work, create after learning something durable.** Both halves are independent.

## Before answering a project question — `query` with mode `filter`

Triggers:

- The user asks "how do we …", "where is …", "what's the convention for …".
- You need to check whether something has already been decided.
- Anything about workflows, architecture, tooling, conventions.

Do this **before** answering, not while drafting your answer. The memories may flip your answer.

```jsonc
// Tool call
{
  "tool": "query",
  "arguments": {
    "mode": "filter",
    "query": "database migration",
    "max_results": 5
  }
}
```

Filter mode requires a positive signal — at least one of `query`, `path`, `logical`, or `tags`. If you pass nothing, you'll get an error. (Use `rank` mode when you want everything ranked against a context.)

Good query strings read like a search someone would type, not like a question:

- ✅ `"jwt token refresh"`
- ✅ `"migrations rollback"`
- ❌ `"how do we handle JWT token refresh in the auth service?"` — too long; the embedding for this drifts away from the indexed summaries.

Filter combos that work well:

- `query` + `type: ["decision"]` — "what did we decide about X?"
- `query` + `tags: ["security"]` — "what do we know about security around X?"
- `query` alone — broadest search; everything semantically near X.

## Before modifying a file — `query` with mode `rank`

Triggers:

- You're about to read, write, or edit a file.
- The user gave you a path or a logical scope.

The PreToolUse hook already does this automatically when wired up via the plugin or `engramdb setup`. You can also call it explicitly when you want more results, a different detail level, or to query before deciding to do an edit.

```jsonc
{
  "tool": "query",
  "arguments": {
    "mode": "rank",
    "path": "src/db/connection.rs",
    "logical": ["database.connection"],
    "max_results": 10
  }
}
```

`mode: "rank"` ranks **every** memory by composite relevance to the given context. It doesn't require a `query` string — the context alone is the signal. Pass a `query` string too if there's a topic; the modes blend.

Why use rank here:

- A file path is rarely a literal substring of any memory summary, so keyword filtering misses things.
- Logical scope contributes a hierarchy-proximity bonus that catches memories about parent / sibling modules.
- You want the agent to see hazards and conventions that apply broadly to the area, not just exact matches.

## After learning something — `create`

Create when you discover a non-obvious convention, hazard, decision, or piece of context that'll help next time. For what counts as worth storing vs noise, see [best-practices.md](./best-practices.md).

Example:

```jsonc
{
  "tool": "create",
  "arguments": {
    "type": "hazard",
    "summary": "LanceDB connector drops table on conflicting schema upsert",
    "content": "Calling Connector::upsert with a schema that differs from the existing table (even by Arrow metadata) silently drops and recreates the table, losing data. Always read the existing schema first and match it exactly.",
    "physical": ["crates/engram-storage/src/lance_index.rs"],
    "logical": ["storage.lancedb"],
    "tags": ["lancedb", "data-loss"],
    "criticality": 0.9
  }
}
```

Field semantics are in [memory-model.md](./memory-model.md); judgment on what makes a memory useful is in [best-practices.md](./best-practices.md).

## Before/after editing — `update`, `supersedes`, and invalidation

A memory's content can drift from reality. When it does:

- **Right answer**: `update` if the original framing is still useful — change `content`, and call `verify` if you re-confirmed it against the code.
- **Right answer for big changes**: `create` a new memory with `supersedes: [<old_id>]`. Supersession closes the old memory's validity window (`invalidated_at` is set, `superseded_by` back-links to the new one): it drops out of default retrieval but stays on disk for history.
- **Right answer when there's no replacement**: `resolve` with `action: "invalidate"` — the claim simply stopped being true. Pass `superseded_by` if a successor exists.
- **Wrong answer**: silently `delete` the old one. You lose the audit trail and may re-derive the same wrong answer later.

A supersession walkthrough:

```jsonc
// 1. The old decision is now wrong — create the replacement, linking it:
{ "tool": "create", "arguments": {
    "type": "decision",
    "summary": "Use SQLite for persistence (reversed PostgreSQL decision, PR #432)",
    "content": "...",
    "supersedes": ["<old_id>"]
} }

// 2. Later, to see the full history including the closed-out version:
{ "tool": "query", "arguments": {
    "mode": "filter", "query": "persistence database choice",
    "epistemic": ["decision"], "include_invalidated": true
} }
```

Invalidated memories carry `invalidated_at` (and `superseded_by` when applicable) in results, so the chain is reconstructible. If you invalidated the wrong memory, `update` with `clear_invalidated: true` reopens the window.

## Task lifecycle — `task_current` and `task_complete`

For memories that only matter while a feature is in flight (plans, temporary constraints, task-local decisions):

1. **Declare the task** at the start of the session: `task_current` with `task: "billing-refactor"`. This maps your session to the task, so task-scoped memories from *other* tasks stay suppressed from hook injection while yours surface. Call it with no `task` to read the current declaration.
2. **Create task-scoped memories** as you work: pass `origin_task: "billing-refactor"` and `generality: "task"` on `create`.
3. **Complete the task** when it ships: `task_complete` with `task: "billing-refactor"`. Task-scoped memories are demoted to fast decay (the 14-day intent curve) so they fade instead of lingering; project-wide memories created for the task are listed back to you with a "verify or demote" notice.

Demotion isn't a death sentence: the maintenance pass watches retrieval telemetry, and when a task-bound memory keeps being retrieved in later sessions (3 distinct sessions by default), it suggests promoting it to project-wide — or does so automatically with `[epistemic] auto_promote = true` (clears `origin_task`, sets `generality: "project"`, restores default decay).

## When you find a contradiction — `challenge`, then `resolve`

If a memory disagrees with what you just observed, **don't update or delete it**. Challenge it with evidence — that records both sides and surfaces the conflict for review.

```jsonc
{
  "tool": "challenge",
  "arguments": {
    "id": "abc1234...",
    "evidence": "src/db/connection.rs now uses SQLite (PR #432); the decision in this memory has been reversed.",
    "source_file": "src/db/connection.rs"
  }
}
```

The memory's status becomes `Challenged`. It stays in the store; the challenge penalty in scoring (`challenge_penalty`, default −0.10) suppresses it without hiding it.

Later (next session, or right then), use `resolve` to decide:

```jsonc
{
  "tool": "resolve",
  "arguments": {
    "id": "abc1234...",
    "action": "update",
    "updated_summary": "We use SQLite for persistence (PR #432 reversed PostgreSQL decision)",
    "updated_content": "..."
  }
}
```

`action` is one of `keep` (re-affirm; clear the challenge), `update` (rewrite), `delete` (the challenge was right; remove), or `invalidate` (it *was* true but no longer is; close the validity window, keeping history — optionally pass `superseded_by`).

If NLI contradiction detection is enabled in config (`[nli].enabled = true`), the server auto-challenges on `create` when a new memory contradicts an existing one. You'll see the auto-challenge in the response.

## Cross-project queries

Most operations take an optional `project` parameter. Pass:

- omit it → current project (the one engramdb resolved from CWD),
- an absolute path → that project,
- a 16-char hex ID → that project (find IDs with `projects_list`),
- `"global"` → the cross-project global store.

```jsonc
// Query the global store
{ "tool": "query", "arguments": { "mode": "filter", "query": "git workflow", "project": "global" } }

// Include global hits in a project query
{ "tool": "query", "arguments": { "mode": "rank", "path": "src/foo.rs", "include_global": true } }
```

Use the global store for cross-cutting preferences and workflows that aren't tied to one codebase.

## Lifecycle: `gc`, `compress_candidates`, `compress_apply`, `review`

Don't run these unprompted. They're maintenance — driven by the user or the system, not your default behavior. See [mcp-tools.md](./mcp-tools.md) for the parameters.

For things to avoid (rank-mode-with-no-context, storing ephemera, delete-vs-supersede), see [best-practices.md](./best-practices.md#anti-patterns-to-avoid).
