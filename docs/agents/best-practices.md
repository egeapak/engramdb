# Best Practices

Concrete rules of thumb for using EngramDB well as an agent. These are derived from how the scoring, storage, and hooks actually behave.

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

- **Short** — ≤ 100 chars hard limit; aim for ~50.
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

## When to update vs create-new vs supersede

**Update** when the original framing is still useful and you're refining details:

```jsonc
{ "tool": "update", "arguments": { "id": "...", "summary": "...", "content": "..." } }
```

**Create new + `supersedes`** when the decision flipped or the conclusion changed:

```jsonc
{
  "tool": "create",
  "arguments": {
    "summary": "Use SQLite for persistence (reverses ADR-007 PostgreSQL choice)",
    "supersedes": ["<old_id>"],
    "...": "..."
  }
}
```

This preserves the audit trail — useful when someone (or future you) is wondering "why did we change?".

**Delete** only when the memory was never valid. If it was true at one point, supersede instead.

## When to challenge

If you find something that disagrees with an existing memory, **always challenge — don't silently update or delete**. The challenge:

- Records both the original claim and your evidence.
- Doesn't lose the original information.
- Surfaces the conflict for the next person to review.

```jsonc
{
  "tool": "challenge",
  "arguments": {
    "id": "<id>",
    "evidence": "src/db/connection.rs:42 now uses SQLite as of PR #432",
    "source_file": "src/db/connection.rs"
  }
}
```

After challenging, either `resolve` it immediately if you're confident, or leave it for human review.

## Sizing content

- **`summary`** ≤ 100 chars. Hard limit.
- **`content`** ~500 tokens soft target. The semantic embedding is computed over `summary + content`, and longer content doesn't help retrieval — the model embeds the first ~256 tokens anyway (`max_tokens`).
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

## Cross-project memories

The global store is for things that genuinely apply across projects:

- Your own workflow preferences (`type: preference`).
- Cross-cutting hazards in tools you use everywhere (`"never run npm audit fix --force"`).
- Reference cards (`"git rebase -i flow we use"`).

Project-specific memories should stay in their project. Don't pollute the global store with stuff that only matters in one place.

## Hook interaction

If the agent is hooked into Claude Code:

- **SessionStart** auto-injects high-criticality memories. Don't re-query the same memories at the start of the session.
- **PreToolUse** auto-runs rank mode against the file path on Read/Write/Edit. You don't need to also call `query` before the same file op — the hook already did.

If you need **more** detail than the hook surfaced (longer content, more results, different scope), explicit `query` calls are fine. Just don't duplicate what the hook handed you already.

## The contradiction-detection bonus

If `[nli].enabled = true` is set in the project's config, `create` automatically checks the new memory against semantically-similar existing ones using an NLI model. Contradictions above the threshold (default 0.7) auto-challenge the conflicting memory.

Two implications:

1. You can `create` more freely — the system will catch some accidental contradictions for you.
2. Watch for auto-challenges in the response of `create`. They contain useful information you should action.

## Anti-patterns to avoid

- **Creating memories that just restate what the user said.** "User wants dark mode" isn't a memory; it's a preference. If it's durable, use `type: preference`. If it's a one-off, don't store it.
- **Creating one memory per file.** Memories are about facts, not files. Multiple files can share a memory; multiple memories can apply to the same file.
- **Using `tags` for everything.** Tags are for things that don't fit `type` or scope. Don't use them to encode the type (use `type` for that) or the scope (use `physical`/`logical` for that).
- **Overlong `summary`.** It will be rejected by validation. If you can't fit it in 100 chars, the memory is too broad — split it.
- **Putting confidential data in memories.** They go into LanceDB (often under the global data dir) and shared memories live in `.engramdb/memories/` which is committed to git. Tokens, passwords, API keys — never.
