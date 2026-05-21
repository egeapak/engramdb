# The Memory Model

Every EngramDB memory is the same struct underneath. This page lists every field and what it does to retrieval.

## Types

The `type` field is one of these eight. They differ in **default decay** and in how a human reading the store will think about them — there's no hard mechanical difference beyond decay.

| Type | What it means | Default decay |
|------|---------------|---------------|
| `decision` | An architectural or design decision that was made. | none |
| `convention` | A coding pattern or rule the team follows. | none |
| `hazard` | A known footgun, bug, or thing not to do. Floor 0.5 (never fully decays). | none, floor 0.5 |
| `context` | Background fact about the codebase that's useful to remember. | none |
| `intent` | An in-flight refactor or planned change. Expected to expire. | exponential, half-life 14 days |
| `relationship` | Connection between components / modules. | none |
| `debug` | A debugging insight or investigation result. Expected to fade. | exponential, half-life 30 days |
| `preference` | User or agent preference. | none |

## Fields

```toml
# TOML frontmatter of a memory file on disk:
id          = "01933d12-..."          # UUID v7 (time-sortable). Auto-assigned.
type        = "decision"               # see table above
title       = "use PostgreSQL"         # short, used in filename; optional
summary     = "Chose PostgreSQL over SQLite for concurrent writes"  # ≤100 chars
content     = "We picked PostgreSQL because ..."   # ~500 tokens
details     = "Long-form details here, lazy-loaded"  # optional
physical    = ["src/db/**"]            # file paths or globs. Default ["/"] (whole project)
logical     = ["database.connection"]  # dot-notation domains
tags        = ["db", "architecture"]   # freeform
criticality = 0.8                       # importance 0..1; default 0.5
confidence  = 0.85                      # certainty 0..1; default 0.8
supersedes  = []                        # other memory IDs this replaces
status      = "active"                  # active | needsreview | challenged
visibility  = "shared"                  # shared | personal
verified_at = "2026-05-20T10:00:00Z"   # last time someone confirmed this is still right
expires_at  = null                      # optional absolute expiration
created_at  = "2026-05-20T09:00:00Z"
updated_at  = "2026-05-20T09:00:00Z"
accessed_at = "2026-05-20T09:00:00Z"

# nested:
[decay]
strategy   = "none"   # none | linear | exponential | step
half_life  = null
ttl        = null
floor      = 0.0

[provenance]
source     = "agent"  # human | agent | inferred | imported
agent_id   = "claude-opus-4-7"
model      = "claude-opus-4-7"
session_id = "..."
reason     = "Discovered while investigating connection pool issues"

# Plus zero or more challenges:
[[challenges]]
evidence    = "..."
challenged_at = "..."
source_file = "..."
```

### Identity and timestamps

- **`id`** — UUID v7 (time-sortable). Auto-assigned. Don't pass `id` on `create`.
- **`created_at`** — set on create. Immutable.
- **`updated_at`** — set on every `update`.
- **`accessed_at`** — set on every query that returns this memory. Used by the daemon for stats.
- **`verified_at`** — set when someone (human or agent) confirmed this memory is still correct (e.g. by `resolve action: keep`). Old `verified_at` is a signal that the memory might be stale.
- **`expires_at`** — optional absolute deadline. After this time the memory is considered expired and filtered out by default (unless `include_expired: true`).

### Content fields

- **`summary`** — the most-searched field. **Make it read like a search query**, not a sentence. Subject + verb + object, ~10 words. Bad: "I think we decided to use PostgreSQL because of concurrent writes." Good: "Use PostgreSQL over SQLite for concurrent writes".
- **`content`** — the body. ~500 tokens. What you'd tell a teammate in one paragraph.
- **`details`** — optional extended details. **Lazy-loaded** — not returned by default in `query` unless you set `detail_level: "full"`. Use for things like full code snippets, links, or extended rationale that's only occasionally needed.
- **`title`** — optional short label that engramdb uses for the on-disk filename (`<slug>_<uuid>.md` if set; just `<uuid>.md` otherwise). Doesn't affect retrieval.

### Scope: `physical` and `logical`

These are the two scoring axes that connect a memory to the code it's about.

- **`physical`** — file paths or globs. `["src/db/**", "tests/db/**"]`. Default `["/"]` (matches anywhere). The PreToolUse hook uses this to find memories when a file is touched.
- **`logical`** — dot-notation domain. `["database.connection", "database.migrations"]`. Hierarchical: parent matches contribute a bonus, sibling matches a smaller bonus.

Scoring contribution:

- Physical: exponential depth decay. `exact_file: 1.0, same_directory: 0.85, same_module: 0.6, project_root: 0.4` (defaults from config).
- Logical bonus: `exact: 0.3, parent: 0.2, sibling: 0.15`. Stacks on top, capped at 1.0.

Set `physical` and `logical` accurately — they're how PreToolUse hooks find relevant memories when an agent touches a file.

### `criticality` and `confidence`

- **`criticality`** (0..1, default 0.5) — importance. High criticality survives GC, appears in session-start hook (default threshold 0.6), and boosts relevance score. For choosing a value, see [best-practices.md](./best-practices.md#how-to-set-criticality).
- **`confidence`** (0..1, default 0.8) — how sure you are. Doesn't affect scoring directly; use ≤0.6 for inferences rather than directly-observed facts.

### `decay`

Decay reduces a memory's effective relevance over time. The `composite_score` formula uses `effective_relevance = criticality × decay_factor(now − updated_at)`.

| Strategy | Behavior |
|----------|----------|
| `none` | Constant. (`floor` still applies if set, e.g. hazards have `floor: 0.5`.) |
| `linear` | Linear from 1.0 at created_at to `floor` over `ttl`. |
| `exponential` | Exponential half-life decay; `decay_factor = max(floor, 0.5^(age/half_life))`. |
| `step` | Step function; full relevance until `ttl`, then `floor`. |

Defaults by type are sensible — don't override unless you have a reason (e.g. a temporary workaround that should fade, or an experiment with a hard expiry).

### `provenance`

Records who/what created the memory. The `source` enum maps to a trust weight in scoring:

| Source | Trust weight (default) | Meaning |
|--------|------------------------|---------|
| `human` | 1.0 | Created by a human directly. |
| `agent` | 0.85 | Created by an agent (you). |
| `inferred` | 0.6 | Inferred from code analysis, not directly observed. |
| `imported` | 0.7 | Pulled in from another source (e.g. ADRs migrated in bulk). |

The trust weight enters the composite formula as `trust_multiplier = floor + (1 - floor) * weight` where the floor (default 0.5) prevents low-trust memories from being suppressed too aggressively.

You don't set `source` directly when calling `create` via MCP — the server sets it to `agent` for you. The other fields (`agent_id`, `model`, `session_id`) are populated automatically from the session.

### `status`

- **`active`** — normal.
- **`needsreview`** — composite score fell below `[thresholds].needs_review` (default 0.3) or someone explicitly set it. Surfaced by `review --stale-only`.
- **`challenged`** — someone called `challenge`. Surfaced by `review --challenged-only`. Suppressed in scoring by `challenge_penalty`.

You generally don't set status directly. Use `challenge` (sets `challenged`) and `resolve` (sets `active`).

### `visibility`

- **`shared`** (default) — lives in `<project>/.engramdb/memories/`. Part of the project, committable, visible to teammates.
- **`personal`** — lives in `<global_data_dir>/projects/<id>/personal/`. Not in the project tree, won't be committed.

Use `personal` for memories that are useful to you but shouldn't be team-visible (debugging notes about your local setup, preferences).

### `supersedes`

A list of IDs of memories that this one **replaces**. When set:

- The superseded memories get marked superseded (they stay around for audit).
- They're filtered out of `query` results by default.
- They appear in the lineage shown by `get`.

Always prefer `supersedes` over `delete` when correcting a memory that was true at some point.

### `challenges`

Auto-appended when `challenge` is called. Each entry records evidence, source file, and timestamp. Don't write to this directly — call `challenge`.

## Validation

These rules are enforced at write time:

- `summary` must be ≤ 100 characters.
- `criticality` must be in [0.0, 1.0].
- `confidence` must be in [0.0, 1.0].
- `decay.floor` must be in [0.0, 1.0].
- `type` must be one of the eight enum values.
- `visibility` must be `shared` or `personal`.
- `status` must be one of the three enum values.

`content` has no hard length limit but soft-targets ~500 tokens. Use `details` for anything longer.

## Update semantics

When you call `update`, every field is **optional** and **replaces** the existing value:

- Scalar fields (`summary`, `content`, `criticality`): replaced if present.
- Vector fields (`physical`, `logical`, `tags`): **replaced** if present.
- For tags only: `tags_add` / `tags_remove` let you make incremental changes without enumerating the full list.

`id`, `created_at`, and `provenance.source` are immutable.
