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
| `include_global?` | `bool` | Also merge global-store hits. Default false. |
| `project?` | `string` | See conventions. |

In `mode: "filter"`, **at least one** of `query`, `path`, `logical`, or `tags` must be present.

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
| `tags?` | `array[string]` | Filter by tags (OR). |
| `status?` | `"active"` \| `"needsreview"` \| `"challenged"` | Filter by status. |
| `scope?` | `string` | Match against physical or logical scope. |
| `sort_field?` | `"criticality"` \| `"created"` \| `"updated"` \| `"type"` | Default `"criticality"`. |
| `reverse?` | `bool` | Reverse sort. |
| `limit?` | `usize` | Cap output. |
| `project?` | `string` | See conventions. |

### `create` (mutates)

**Store a new memory.** Use after discovering patterns, decisions, or hazards.

| Param | Type | Description |
|-------|------|-------------|
| `type` | `string` | One of: `decision`, `convention`, `hazard`, `context`, `intent`, `relationship`, `debug`, `preference`. Required. |
| `content` | `string` | Main body (~500 tokens). Required. |
| `summary` | `string` | One-line summary, ≤ 100 chars. Required. |
| `details?` | `string` | Extended details (lazy-loaded). |
| `physical?` | `array[string]` | File paths or globs. Default `["/"]`. |
| `logical?` | `array[string]` | Dot-notation domains. |
| `tags?` | `array[string]` | Freeform tags. |
| `criticality?` | `f64` | 0..1, default 0.5. |
| `confidence?` | `f64` | 0..1, default 0.8. |
| `visibility?` | `"shared"` \| `"personal"` | Default `"shared"`. |
| `supersedes?` | `array[string]` | IDs this memory replaces. |
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
| `supersedes?` | `array[string]` | |
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

**Resolve a challenged or needs-review memory.** Three actions:

| Param | Type | Description |
|-------|------|-------------|
| `id` | `string` | Required. |
| `action` | `"keep"` \| `"update"` \| `"delete"` | Required. |
| `updated_content?` | `string` | Required when `action: "update"`. |
| `updated_summary?` | `string` | Optional with `action: "update"`. |
| `project?` | `string` | See conventions. |

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

**Merge multiple memories into one summary.** Source memories are deleted; the new compressed memory `supersedes` them.

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

### `doctor` (read-only)

**Fast store health check** (index vs disk consistency).

| Param | Type | Description |
|-------|------|-------------|
| `project?` | `string` | See conventions. |

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
