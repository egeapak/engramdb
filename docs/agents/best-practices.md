# Best Practices

Rules of thumb for using EngramDB well.

## What to remember

**Remember the durable, non-obvious, project-specific.** A good memory is something a fresh agent walking into this project couldn't infer from the code in five minutes.

✅ Good candidates:

- An architectural decision that has alternatives (and which one was chosen).
- A footgun that only shows up under specific conditions.
- A convention that the team follows but the codebase doesn't enforce.
- A relationship between two components that isn't obvious from imports.
- The reason a piece of code is the way it is (the "why", which code can't store).

❌ Bad candidates:

- Things already documented in code comments, docstrings, or README — those don't drift from the code.
- One-off facts that won't help next time.
- Narration of your own thought process.
- The user's current question.
- "Maybe we should…" — wait until the decision is real.
- Things you can re-derive in two minutes by reading the code.

## How to phrase a summary

The `summary` is the most-searched field. It's tokenized against semantic queries and shown in every result. Write it to be:

- **Short** — ≤ 200 chars hard limit (configurable); aim for ~50.
- **Search-shaped** — subject + verb + object. Imagine someone typing this into Google.
- **Self-contained** — no pronouns or references that require context to understand.

| ❌ Bad | ✅ Good |
|--------|---------|
| "Database choice" | "Use PostgreSQL over SQLite for concurrent writes" |
| "Auth tokens" | "JWT refresh tokens rotate every 7 days via background job" |
| "Migrations" | "Migrations live in src/db/migrations and run via diesel" |
| "I noticed that the connection pool leaks when..." | "Connection pool leaks when LanceDB session is dropped without close()" |

## How to set `criticality`

Default is `0.5`. Bump up or down based on the cost of forgetting:

| Criticality | When |
|-------------|------|
| 0.95 | Forgetting this causes data loss / production outage. |
| 0.85 | Forgetting this causes silent correctness bugs. |
| 0.75 | Forgetting this causes embarrassing rework. |
| 0.5 | Useful background. Default. |
| 0.3 | Minor hint. Probably not worth a memory. |

The session-start hook surfaces memories at `criticality ≥ 0.6` by default. So `0.6+` ⇒ "the user will see this when they start a session". Use that responsibly — don't fill the session-start surface with low-value memories.

## How to set `physical` and `logical`

These are how the PreToolUse hook finds your memory when the agent touches a file.

**Physical** = file paths or globs the memory is about:

- ✅ `["src/db/migrations/**"]` — the memory applies to anything under that dir.
- ✅ `["src/db/connection.rs"]` — exact file.
- ✅ `["src/api/auth/**", "src/api/middleware/auth.rs"]` — multiple locations.
- ❌ `["/"]` — default; means "anywhere in the project". Use only when the memory genuinely is project-wide.
- ❌ `["**/*.rs"]` — too broad.

**Logical** = dot-notation domain:

- ✅ `["database.connection"]`
- ✅ `["auth.oauth.pkce", "auth.session"]`
- ✅ `["build.ci"]`

Build a consistent vocabulary inside a project. If you've used `database.migrations` before, don't switch to `db.migrations` later — search first, reuse second.

Logical scoring is hierarchical: a memory tagged `database` matches a query in `database.connection` (parent bonus). A memory tagged `database.migrations` does not match `database.connection` (siblings — smaller bonus).

## Epistemic hygiene

Field semantics are in [memory-model.md](./memory-model.md#epistemic-classes); this is the judgment.

**State the falsifier when you know it.** For decisions and observations, ask: "what change would make this wrong?" If you can name it, record it:

- `premise` — the condition the memory depends on. ✅ `"while we pin ort rc.12"`, `"while SQLite is the only backend"`. It's surfaced verbatim in results, so a future agent can check it in seconds.
- `invalidated_by` — paths/globs whose change invalidates the memory. This is **not** `physical` (where the memory applies): a benchmark observation may apply to `src/retrieval/` but be invalidated by `Cargo.lock` changing. Declaring it gets you real-time warnings when the watched path is edited, and `doctor` findings when it drifts.

Facts usually need neither — they're verifiable against the repo directly.

**Scope task-bound memories to the task.** When a memory only matters for the feature you're building (an in-flight plan, a temporary constraint), set `origin_task` plus `generality: "task"`. It stays out of other sessions' hook injections, and `task_complete` demotes it to fast decay instead of letting it linger. Declare `task_current` at the start of a work session and call `task_complete` when the task ships — see [workflows.md](./workflows.md#task-lifecycle--task_current-and-task_complete).

**Prefer `resolve` with `action: "invalidate"` over `delete`** when a memory *was* true but no longer is. Invalidation closes the validity window: the memory drops out of default retrieval but stays queryable via `include_invalidated`, so the history (and the reason it changed) survives. Delete only what was never true or is noise.

**Use `verify` after re-confirming.** When you check a memory against the code and it still holds, call `verify` — it stamps `verified_at` (facts rank fresher from that anchor) and clears a doctor-flagged review. Don't rewrite a correct memory just to "refresh" it.

## Sizing content

- **`summary`** ≤ 200 chars by default (configurable via `[content].summary_max_chars`). Hard limit.
- **`content`** ~500 tokens soft target. The semantic embedding is computed over `summary + content`. Longer content is **not** lost: it's split into ~`max_tokens`-sized chunks (256 by default) and every chunk is embedded and searched independently. Keep the main point in `content` and push bulk detail to `details` (which is not embedded) so retrieval stays focused rather than diluted across many chunks.

> These limits, the retrieval thresholds, and the store's most-used tags are all readable at runtime via the [`config`](./mcp-tools.md#config-read-only) tool — call it once at the start of a session rather than hard-coding the numbers above.
- **`details`** anything longer. Lazy-loaded — only fetched when `detail_level: "full"`. Use for code snippets, long rationale, links.

If you find yourself writing > 1000 tokens of `content`, you almost always want to split:

- The high-level fact → `content`
- The detail → `details`

Or even better, split into multiple memories with different scopes.

## Query economy

Don't query repeatedly for variants of the same thing. EngramDB doesn't paginate within a session — pick `max_results` once and broaden it.

❌ Three calls for the same topic:
```
query "auth oauth" → 3 results
query "authentication oauth" → 3 results
query "OAuth flow" → 3 results
```

✅ One call:
```
query "oauth authentication flow" → 10 results
```

Semantic similarity catches the variants. Trust it.

## What goes in the global store

The global store (`project: "global"`) is for things that apply across projects: workflow preferences, cross-cutting hazards in tools you use everywhere, reference cards. Project-specific memories stay in the project. See [workflows.md](./workflows.md#cross-project-queries) for the mechanics.

## Hook interaction

- **SessionStart** auto-injects high-criticality memories. Don't re-query the same memories at session start.
- **PreToolUse** auto-runs rank mode against the file path on Read/Write/Edit. Don't redundantly call `query` for the same file op — the hook already did. Explicit calls are fine when you need more detail or a different scope.

## Anti-patterns to avoid

- **Creating memories that just restate what the user said.** "User wants dark mode" isn't a memory; it's a preference. If it's durable, use `type: preference`. If it's a one-off, don't store it.
- **Creating one memory per file.** Memories are about facts, not files. Multiple files can share a memory; multiple memories can apply to the same file.
- **Using `tags` for everything.** Tags are for things that don't fit `type` or scope. Don't use them to encode the type (use `type` for that) or the scope (use `physical`/`logical` for that).
- **Overlong `summary`.** It will be rejected by validation. If you can't fit it in the configured limit (200 chars by default), the memory is too broad — split it.
- **Putting confidential data in memories.** They go into LanceDB (often under the global data dir) and shared memories live in `.engramdb/memories/` which is committed to git. Tokens, passwords, API keys — never.
