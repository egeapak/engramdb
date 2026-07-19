# Query Modes: `filter` vs `rank`

The `mode` parameter on `query` is the most important decision.

## TL;DR

| Use `filter` when… | Use `rank` when… |
|--------------------|------------------|
| The user asked a question with keywords. | You're about to touch a specific file or scope. |
| You're checking if a specific decision/convention exists. | You want to see what's broadly relevant to a context. |
| You have a topic in mind. | You don't have keywords — just a place. |
| You want a small, precise set. | You want a ranked window of everything related. |

## `filter` mode

> "Find memories matching this signal."

```jsonc
{
  "tool": "query",
  "arguments": {
    "mode": "filter",
    "query": "jwt refresh token rotation",
    "max_results": 5
  }
}
```

**Requires at least one of**: `query`, `path`, `logical`, or `tags`. Empty filter is rejected.

How it works:

1. Apply hard filters (`types`, `tags`, `min_criticality`).
2. Compute a relevance signal from whichever of `query`, `path`, `logical`, `tags` you supplied.
3. Return only memories above `[retrieval].relevance_threshold`.
4. Sort by composite score, take top `max_results` (default 10).

Use it for:

- Answering a user question by keyword (`query`).
- Pulling all memories about a specific scope (`logical: ["auth.oauth"]`).
  Logical matching is **hierarchical**, mirroring rank mode's dot-notation
  semantics: querying `auth` matches memories scoped `auth`, `auth.oauth`,
  `auth.oauth.google` (descendants), and querying `auth.oauth` also matches a
  memory scoped just `auth` (broad memories apply to their subdomains).
  Siblings (`auth.jwt` vs `auth.oauth`) and lookalike prefixes
  (`authentication` vs `auth`) do **not** match.
- Pulling memories by tag set (`tags: ["security"]`).
- Combinations (`query` + `tags` is a powerful narrow search).

Anti-pattern: passing `query: ""` or no signals at all. It will error. Use `rank` mode if you don't have a signal.

## `rank` mode

> "Rank everything by how relevant it is to this context."

```jsonc
{
  "tool": "query",
  "arguments": {
    "mode": "rank",
    "path": "src/api/auth/handlers.rs",
    "logical": ["auth.oauth"],
    "max_results": 10
  }
}
```

No required signal. You can pass nothing and get a top-N by raw relevance (criticality × decay) — but that's rarely useful.

How it works:

1. Apply hard filters (`types`, `tags`, `min_criticality`).
2. Score **every** remaining memory by composite formula:
   - relevance (criticality × decay)
   - × scope multiplier (depth-decayed match on `path` + logical-hierarchy bonus on `logical`;
     with `logical` only, the bonus rides on `scope_multiplier_floor` (default 0.5) so related
     memories rank above the threshold, unscoped memories sit at the bare floor, and memories
     with unrelated logical scopes drop to 0)
   - × trust multiplier (`Provenance` source)
   - − challenge penalty (if challenged)
   - + semantic similarity (if `query` is also passed)
3. Sort by score, return top `max_results`.

Use it for:

- The PreToolUse path: "I'm about to edit `src/foo.rs` — what should I know?"
- Browsing what's known about a scope without a specific question.
- The session-start surface (what's hot right now?).

`rank` mode never errors on missing signals — it always returns something, even if the something is uninformative without a path.

## When both could work

If you have both keywords **and** a file context, `rank` mode with both `query` and `path` is the better choice — you get keyword relevance plus scope proximity, blended. `filter` mode with just `query` would miss memories whose summary doesn't mention the keyword but whose scope matches.

Example: user asks "how does auth work" while pointed at `src/api/auth/oauth.rs`. Use `rank` with both `query: "auth"` and `path: "src/api/auth/oauth.rs"` — you'll catch the design decision (matches the keyword) **and** the file-specific hazard (matches the path).

## Parameters that apply to both modes

| Parameter | Effect |
|-----------|--------|
| `types: ["decision", "hazard"]` | Hard filter; only those types are considered. |
| `epistemic: ["fact", "decision"]` | Hard filter by epistemic class; OR within the list, like `types`. |
| `tags: ["security", "auth"]` | Hard filter; OR within the list. |
| `min_criticality: 0.5` | Drops memories below this. |
| `max_results: 10` | Result cap (default 10). |
| `detail_level: "summary"\|"content"\|"full"` | How much of each memory to return (default `content`). |
| `include_expired: false` | Set true to include decayed memories. |
| `include_invalidated: false` | Set true to include memories whose validity window was closed (superseded / invalidated). |
| `situation: "..."` | Reweights epistemic classes for your current activity. See below. |
| `include_global: false` | Set true to merge global-store hits. |
| `project: "..."` | Target a different project. |

## Parameters specific to each mode

| Parameter | `filter` | `rank` |
|-----------|----------|--------|
| `query` | At least one signal required | Optional; blends with scope |
| `path` | Filters or scopes | Scoring signal (proximity) |
| `logical` | Filters or scopes | Scoring signal (hierarchy) |

(In `rank` mode, `path` and `logical` act as **scoring** signals. In `filter` mode they are hard filters: `logical` is matched **hierarchically** — a memory passes when any of its logical scopes is equal to, a descendant of, or an ancestor of a queried scope on the same dot-notation chain.)

## Situation-aware queries

The `situation` parameter tells retrieval what you're doing so it can reweight memories by **epistemic class** (`fact` / `observation` / `decision` — see [memory-model.md](./memory-model.md#epistemic-classes)). It works in both modes and is a soft reweight, not a filter: the multiplier is `floor + (1 − floor) × profile[situation][class]` with a floor of 0.6 by default, so a class is down-weighted at most 40%, never hidden.

Default profiles (higher = ranks higher):

| Situation | fact | observation | decision | Set by |
|-----------|------|-------------|----------|--------|
| `session_start` | 1.0 | 0.5 | 0.8 | SessionStart hook — static facts and project-wide decisions matter most. |
| `file_edit` | 0.7 | 0.7 | 1.0 | PreToolUse hook — decisions (and hazards) binding on the file dominate. |
| `debugging` | 0.6 | 1.0 | 0.7 | You, when investigating a failure — observations rank highest. |
| `design_choice` | 0.8 | 0.7 | 1.0 | You, when weighing alternatives — prior decisions and their rationale dominate. |

The hooks set `session_start` and `file_edit` automatically; declare `debugging` and `design_choice` yourself — hooks can't detect them. Omitting `situation` is neutral (multiplier 1.0). Each result's `score_breakdown.situation_multiplier` shows the applied value.

Two related parameters are **hard** controls, not reweights:

- `epistemic: ["observation"]` — only those classes are considered (OR within the list, like `types`).
- `include_invalidated: true` — also return memories whose validity window was closed (superseded / invalidated). Default false: closed-window memories are excluded from results entirely.

## Examples

**User asks a conventions question:**

```jsonc
{ "mode": "filter", "query": "where do we put SQL migrations", "types": ["convention", "context"] }
```

**About to edit a file:**

```jsonc
{ "mode": "rank", "path": "src/db/migrations/0042_user_email_index.sql" }
```

**Investigating an area before answering a design question:**

```jsonc
{ "mode": "rank", "query": "rate limiting strategy", "logical": ["api.middleware"], "max_results": 15 }
```

**Searching by tag (e.g., looking for hazards in security):**

```jsonc
{ "mode": "filter", "tags": ["security"], "types": ["hazard"], "min_criticality": 0.6 }
```

**Surface high-criticality memories for orientation (session-start-like):**

```jsonc
{ "mode": "rank", "min_criticality": 0.7, "max_results": 5 }
```

**Investigating a test failure (observations rank highest):**

```jsonc
{ "mode": "filter", "query": "flaky test model load race", "situation": "debugging" }
```

**Reviewing the history of a decision, including superseded versions:**

```jsonc
{ "mode": "filter", "query": "embedding backend choice", "epistemic": ["decision"], "include_invalidated": true }
```

## Common mistakes

- **`mode: "filter"` with no signal.** Errors out. Switch to `rank`, or supply a signal.
- **`mode: "rank"` with no path or query.** Works, but ranks by raw criticality × decay only — usually not what you want.
- **Treating `path` as a filter.** It's a scoring signal in both modes; memories without a matching scope still score against it, just lower.
