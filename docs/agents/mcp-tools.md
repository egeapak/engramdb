# MCP Tool Reference

Every tool exposed by `engramdb serve`. For when to use which tool, see [workflows.md](./workflows.md) and [query-modes.md](./query-modes.md); for field semantics, see [memory-model.md](./memory-model.md).

## Conventions used here

- Every tool that operates on memories accepts an optional `project` parameter:
  - omit it → current project,
  - absolute path → that project,
  - 16-char hex ID → that project (find IDs with `projects_list`),
  - `"global"` → the cross-project global store.
- Type annotations: `string`, `f64`, `usize`, `u64`, `bool`, `array[T]`. `?` after a parameter name means optional.

---

## Memory tools

### `query` (read-only)

**Search or browse memories.** The single retrieval entry point.

| Param | Type | Description |
|-------|------|-------------|
| `mode` | `"rank"` \| `"filter"` | Required. `"rank"` browses by context; `"filter"` requires a positive signal. |
| `query?` | `string` | Search text (tokenized against summary, content, tags). |
| `path?` | `string` | Physical context (file path) for proximity scoring. |
| `logical?` | `array[string]` | Logical scopes (dot-notation). Scoring signal in `rank` mode; in `filter` mode a hard **hierarchical** filter — a memory matches when one of its scopes is equal to, a descendant of, or an ancestor of a queried scope (querying `auth` matches `auth.oauth`; siblings like `auth.jwt` vs `auth.oauth` do not match). |
| `types?` | `array[string]` | Filter by memory types. |
| `tags?` | `array[string]` | Filter by tags (OR within). |
| `min_criticality?` | `f64` | Drop memories below this criticality. |
| `max_results?` | `usize` | Default 10. |
| `detail_level?` | `"summary"` \| `"content"` \| `"full"` | Default `"content"`. Controls how much of each memory is returned. |
| `include_expired?` | `bool` | Default false. |
| `epistemic?` | `array[string]` | Hard filter by epistemic class: `fact`, `observation`, `decision` (OR logic, like `types`). |
| `situation?` | `string` | `session_start` \| `file_edit` \| `debugging` \| `design_choice`. Reweights epistemic classes for what you're doing (see [query-modes.md](./query-modes.md#situation-aware-queries)). Declare `debugging` when investigating a failure, `design_choice` when weighing alternatives. |
| `include_invalidated?` | `bool` | Default false. Include memories whose validity window was closed (superseded / invalidated). |
| `include_global?` | `bool` | Also merge global-store hits. Default false. |
| `project?` | `string` | See conventions. |

In `mode: "filter"`, **at least one** of `query`, `path`, `logical`, or `tags` must be present.

Every returned memory carries `epistemic` (always) plus `valid_while`, `valid_from`, `invalidated_at`, `superseded_by`, and `verified_at` when present. Each result's `score_breakdown` includes `situation_multiplier` (1.0 when no `situation` was passed).

### `get` (read-only)

**Fetch full content of a specific memory, including details.**

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Memory ID. Prefix matching is supported. |
| `project?` | `string` | See conventions. |

Returns the full memory object including `details` (which is lazy-loaded in `query`).

### `list` (read-only)

**List memories with filtering, sorting, and limiting.**

| Param | Type | Description |
|-------|------|-------------|
| `types?` | `array[string]` | Filter by types. |
| `epistemic?` | `array[string]` | Filter by epistemic class: `fact`, `observation`, `decision` (OR). |
| `tags?` | `array[string]` | Filter by tags (OR). |
| `status?` | `"active"` \| `"needsreview"` \| `"challenged"` | Filter by status. |
| `scope?` | `string` | Match against physical or logical scope. |
| `sort_field?` | `"criticality"` \| `"created"` \| `"updated"` \| `"type"` | Default `"criticality"`. |
| `include_invalidated?` | `bool` | Default false. Include memories whose validity window was closed. |
| `reverse?` | `bool` | Reverse sort. |
| `limit?` | `usize` | Cap output. |
| `project?` | `string` | See conventions. |

Each entry includes `epistemic` (always) and `invalidated_at` / `valid_from` when present.

### `create` (mutates)

**Store a new memory.** Use after discovering patterns, decisions, or hazards.

| Param | Type | Description |
|-------|------|-------------|
| `type` | `string` | One of: `decision`, `convention`, `hazard`, `context`, `intent`, `relationship`, `debug`, `preference`. Required. |
| `content` | `string` | Main body (~500 tokens). Required. |
| `summary` | `string` | One-line summary, ≤ 200 chars by default (configurable via `[content].summary_max_chars`). Required. |
| `details?` | `string` | Extended details (lazy-loaded). |
| `physical?` | `array[string]` | File paths or globs. Default `["/"]`. |
| `logical?` | `array[string]` | Dot-notation domains. |
| `tags?` | `array[string]` | Freeform tags. |
| `criticality?` | `f64` | 0..1, default 0.5. |
| `confidence?` | `f64` | 0..1, default 0.8. |
| `visibility?` | `"shared"` \| `"personal"` | Default `"shared"`. |
| `supersedes?` | `array[string]` | IDs this memory replaces (closes their validity windows). |
| `epistemic?` | `"fact"` \| `"observation"` \| `"decision"` | Epistemic class. Defaults from `type`; set only when it differs. |
| `premise?` | `string` | Premise this memory depends on, e.g. `"while we pin ort rc.12"`. State it if the memory becomes wrong when something specific changes. |
| `invalidated_by?` | `array[string]` | Paths/globs whose change invalidates this memory (distinct from `physical`, which is where it applies). |
| `origin_task?` | `string` | Task/feature this was decided for (short human-readable name, not a session ID). |
| `generality?` | `"project"` \| `"task"` | Default `"project"`. `"task"` = binding only within `origin_task`. |
| `valid_from?` | `string` | Valid-time start (RFC3339). Only to backdate; defaults to creation time. |
| `decay_strategy?` | `"none"` \| `"linear"` \| `"exponential"` \| `"step"` | Override default for type. |
| `decay_half_life?` | `u64` | Seconds, for exponential. |
| `decay_ttl?` | `u64` | Seconds, for linear/step. |
| `decay_floor?` | `f64` | Min decay factor 0..1. |
| `title?` | `string` | Short title used in the file name. |
| `title_strategy?` | `"keyword"` \| `"t5"` \| `"none"` | Title generation strategy if `title` omitted. Defaults to the project's `[title].strategy` config (`t5` unless overridden). |
| `project?` | `string` | See conventions. |

If NLI contradiction detection is enabled, the response may include auto-challenges against conflicting existing memories.

### `update` (mutates)

**Modify an existing memory.** Cannot change `id` or `created_at`.

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Required. Prefix matching supported. |
| `type?` | `string` | New type. |
| `content?` | `string` | Replace content. |
| `summary?` | `string` | Replace summary. |
| `details?` | `string` | Replace details. |
| `physical?` | `array[string]` | **Replaces** physical scope. |
| `logical?` | `array[string]` | **Replaces** logical scope. |
| `tags?` | `array[string]` | **Replaces** all tags. |
| `tags_add?` | `array[string]` | Incrementally add tags (merged with existing). |
| `tags_remove?` | `array[string]` | Incrementally remove tags. |
| `criticality?` | `f64` | |
| `confidence?` | `f64` | |
| `visibility?` | `"shared"` \| `"personal"` | |
| `status?` | `"active"` \| `"needsreview"` \| `"challenged"` | |
| `title?` | `string` | |
| `supersedes?` | `array[string]` | Closes the listed memories' validity windows. |
| `epistemic?` | `"fact"` \| `"observation"` \| `"decision"` | |
| `premise?` | `string` | Merged into the existing validity condition. |
| `invalidated_by?` | `array[string]` | Merged into the existing validity condition. |
| `origin_task?` | `string` | Merged into the existing validity condition. |
| `generality?` | `"project"` \| `"task"` | Merged into the existing validity condition. |
| `valid_from?` | `string` | Valid-time start (RFC3339). |
| `clear_validity?` | `bool` | Clear the whole validity condition (premise/invalidated_by/origin_task/generality). Wins over piecemeal validity edits in the same call. |
| `clear_invalidated?` | `bool` | Reopen a closed validity window: clears `invalidated_at` and `superseded_by`. Invalidation is reversible, unlike deletion. |
| `decay_strategy?` | `string` | |
| `decay_half_life?` | `u64` | |
| `decay_ttl?` | `u64` | |
| `decay_floor?` | `f64` | |
| `project?` | `string` | See conventions. |

### `delete` (mutates)

**Permanently delete a memory.**

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Required. |
| `project?` | `string` | See conventions. |

---

## Challenge / review tools

### `challenge` (mutates)

**Flag a memory as potentially incorrect.** Status becomes `challenged`.

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Required. |
| `evidence` | `string` | Required. What contradicts the memory. |
| `source_file?` | `string` | File where evidence was found. |
| `project?` | `string` | See conventions. |

### `review` (read-only)

**List memories needing review** — flagged (`challenged` / `needsreview`) plus, via the recency trigger, active memories not updated in more than `[review].recency_days` (default 90). Results are highest-criticality first, and the response echoes the effective window as `recency_days`.

| Param | Type | Description |
|-------|------|-------------|
| `scope?` | `string` | Filter to a scope. |
| `max_results?` | `usize` | Default 10. |
| `type?` | `string` | Filter by memory type. |
| `challenged_only?` | `bool` | Only `challenged`. |
| `stale_only?` | `bool` | Only `needsreview`. |
| `stale_after_days?` | `u64` | Recency window override in days. Omit to use `[review].recency_days`; `0` disables the recency arm (flagged only). |
| `project?` | `string` | See conventions. |

The `memory-session-end` prompt also reports how many active memories are past the recency window, so an agent can proactively offer to revisit them.

### `resolve` (mutates)

**Resolve a challenged or needs-review memory.** Four actions:

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Required. |
| `action` | `"keep"` \| `"update"` \| `"delete"` \| `"invalidate"` | Required. |
| `updated_content?` | `string` | Required when `action: "update"`. |
| `updated_summary?` | `string` | Optional with `action: "update"`. |
| `superseded_by?` | `string` | With `action: "invalidate"`: ID of the memory that superseded this one (optional). |
| `project?` | `string` | See conventions. |

`action: "invalidate"` closes the memory's validity window (`invalidated_at = now`) instead of deleting it. Prefer `invalidate` over `delete` when a memory **was** true but no longer is — history is kept and queryable via `include_invalidated`.

### `verify` (mutates)

**Confirm a memory is still accurate** after checking it against the code. Stamps `verified_at` (fact-class memories decay from this anchor, so they rank fresher) and clears a doctor-flagged `needsreview` status.

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Required. |
| `project?` | `string` | See conventions. |

Returns `{ id, verified, review_cleared }`.

---

## Task tools

### `task_current` (read/write session state)

**Declare the task/feature this session is working on.** Task-scoped memories (`generality: "task"`) from other tasks stay suppressed from hook injection; yours surface. Call without `task` to read the current declaration.

| Param | Type | Description |
|-------|------|-------------|
| `task?` | `string` | Task/feature name to declare (short human-readable name). Omit to read the current declaration. |
| `project?` | `string` | See conventions. |

### `task_complete` (mutates)

**Mark a task/feature finished.** Task-scoped memories (`valid_while.origin_task` matching, `generality: "task"`) are demoted to fast decay (the 14-day intent curve) unless they carry an explicit custom decay; project-wide memories from the task are listed for review ("verify or demote").

| Param | Type | Description |
|-------|------|-------------|
| `task` | `string` | Required. Task/feature name to mark finished. |
| `project?` | `string` | See conventions. |

Returns `{ task, demoted, kept_custom_decay, project_wide_review }`.

---

## Lifecycle tools

### `compress_candidates` (read-only)

**Find low-criticality memories eligible for compression.** Review before calling `compress_apply`.

| Param | Type | Description |
|-------|------|-------------|
| `scope?` | `string` | Filter by logical scope. |
| `threshold?` | `f64` | Criticality threshold. Default 0.4. |
| `project?` | `string` | See conventions. |

### `compress_apply` (mutates)

**Merge multiple memories into one summary.** The new compressed memory `supersedes` the sources, which closes their validity windows (they stay on disk, queryable via `include_invalidated`, and are purged by `gc` only after the retention window).

| Param | Type | Description |
|-------|------|-------------|
| `source_ids` | `array[string]` | Required. IDs to compress into one. |
| `summary` | `string` | Required. Summary for the new memory. |
| `content` | `string` | Required. Content for the new memory. |
| `scope?` | `array[string]` | Logical scopes for the new memory. |
| `tags?` | `array[string]` | Tags for the new memory. |
| `project?` | `string` | See conventions. |

### `gc` (mutates)

**Garbage-collect decayed memories.**

| Param | Type | Description |
|-------|------|-------------|
| `dry_run?` | `bool` | Default true. List only, no delete. |
| `threshold?` | `f64` | Override `[thresholds].gc` (default 0.05). |
| `project?` | `string` | See conventions. |

### `reindex` (mutates)

**Rebuild the search index and embedding vectors.**

| Param | Type | Description |
|-------|------|-------------|
| `embeddings_only?` | `bool` | Only re-embed, skip index rebuild. |
| `index_only?` | `bool` | Only rebuild index, skip embedding. |
| `project?` | `string` | See conventions. |

---

## Inspection tools

### `stats` (read-only)

**Counts and aggregates.**

| Param | Type | Description |
|-------|------|-------------|
| `all_projects?` | `bool` | Include per-project runtime telemetry breakdown. |
| `project?` | `string` | See conventions. |

### `config` (read-only)

**Effective config values and store vocabulary** — call this once at the start of a session to learn the limits and thresholds that govern the other tools, and to get a feel for what is already in memory. Returns:

- `limits` — `summary_max_chars` (hard cap `create` enforces), `content_soft_token_target`, and `embedding_chunk_tokens` (per-chunk embedding window; content past one window is chunked and still embedded, not truncated).
- `retrieval` — `default_max_results`, `relevance_threshold` (min score a `query` result clears), `search_threshold`, `search_semantic_weight`, `include_expired`.
- `features` — `rerank_enabled` / `rerank_top_n`, `contradiction_detection_enabled` (whether `challenge`'s NLI is on), and the `title_strategy` used when `create` gets no title.
- `embedding` — `provider` and vector `dimensions`.
- `top_tags` — the most-used unique tags with counts, most-used first.

| Param | Type | Description |
|-------|------|-------------|
| `top_tags?` | `int` | Number of top tags to include (most-used first). Default 20. |
| `project?` | `string` | See conventions. |

### `doctor` (read-only unless `fix: true`)

**Fast store health check** (index vs disk consistency) plus epistemic checks: watched paths (`invalidated_by`) changed since last verification, observations unverified past `[epistemic].observation_review_days` (default 90), and memories whose `derived_from` sources are missing or invalid.

| Param | Type | Description |
|-------|------|-------------|
| `fix?` | `bool` | Default false (report only). When true, flips memories with epistemic findings (changed invalidation paths, invalid derived-from sources) to `needsreview`. Stale observations are reported but never flipped. |
| `project?` | `string` | See conventions. |

Epistemic findings appear as `epistemic_findings` in the response. Use `verify` to clear a doctor-flagged memory after re-confirming it.

---

## Project registry tools

These manage the cross-project registry rather than individual memories. Useful for discovering IDs and managing project hierarchies.

### `projects_list` (read-only)

**List all registered projects** with hierarchy. Discovers 16-char project IDs.

No parameters.

### `projects_info` (read-only)

**Info about a specific project**: id, name, path, memory count, logical scopes, created_at, parent_project_id.

| Param | Type | Description |
|-------|------|-------------|
| `project?` | `string` | Target project. Omit for current. |

### `projects_link` (mutates)

**Link a project as a sub-project of another.** Rejects self-links and cycles.

| Param | Type | Description |
|-------|------|-------------|
| `child` | `string` | Required. 16-char project ID. |
| `parent` | `string` | Required. 16-char project ID. |

### `projects_unlink` (mutates)

**Promote a sub-project back to a root.** No-op if it had no parent.

| Param | Type | Description |
|-------|------|-------------|
| `project_id` | `string` | Required. 16-char project ID. |

---

## Transports

The server supports two transports:

- **stdio** (default for Claude Code): one process per agent session.
- **HTTP/SSE**: streamable HTTP for cases where you want a long-lived server with per-connection MCP instances. Start with `engramdb serve --transport sse --port <N>`.

The session ID is sourced from `CLAUDE_SESSION_ID` or `MCP_SESSION_ID` env var, falling back to a fresh UUID per process. This is the same session ID that appears in `Provenance.session_id` for memories created during the session.
