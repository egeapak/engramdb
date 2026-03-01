# EngramDB — Design Specification

**Version:** 0.1.0-draft
**Date:** 2026-02-10
**Status:** RFC / Design Phase

---

## 1. Problem Statement

Coding agents (Claude Code, Cursor, Copilot, Aider, etc.) operate with limited context windows and no persistent memory across sessions. When an agent revisits a codebase, it loses all accumulated knowledge — architectural decisions, known hazards, conventions, in-flight refactors, and hard-won debugging insights.

Current workarounds (CLAUDE.md, CONVENTIONS.md, inline comments) are flat, unstructured, and don't support scoping, prioritization, or decay. They also grow unbounded and stale.

**Goal:** A lightweight, file-based EngramDB that coding agents can read and write, scoped to projects and modules, with built-in mechanisms for prioritization, staleness, and discoverability.

---

## 2. Design Principles

1. **File-based and portable** — No database, no daemon. The store lives in the repo as files that can be version-controlled, reviewed, and edited by humans.
2. **Agent-first, human-readable** — Optimized for machine consumption but fully transparent to developers.
3. **Scoped, not flat** — Memories are attached to physical paths and logical domains.
4. **Opinionated about relevance** — Criticality and decay are first-class, so retrieval surfaces the right memories, not all of them.
5. **Protocol-native** — Designed to work as an MCP server, CLI tool, or direct file access.

---

## 3. Core Concepts

### 3.1 Memory

A **Memory** is the atomic unit — a discrete piece of knowledge about the project.

```
Memory {
  id:           string       // UUID v7 (time-sortable)
  type:         MemoryType   // Categorization (see §3.2)
  summary:      string       // One-line summary, <100 chars (required, in index)
  content:      string       // Core knowledge, <500 tokens (loaded with memory)
  details:      string?      // Extended context, unlimited (lazy-loaded on demand)

  // --- Scoping ---
  physical:     string[]     // File/folder paths this applies to (glob-capable)
  logical:      string[]     // Logical scopes: module, feature, layer, domain

  // --- Metadata ---
  tags:         string[]     // Freeform tags for flexible querying
  criticality:  float        // 0.0 (trivial) to 1.0 (critical / must-know)

  // --- Lifecycle ---
  decay:        Decay?       // Optional decay configuration (see §3.3)
  provenance:   Provenance   // Who/what created this and when (see §3.4)
  confidence:   float        // 0.0 (speculation) to 1.0 (verified fact)
  supersedes:   string[]     // IDs of memories this replaces

  // --- Timestamps ---
  created_at:   datetime
  updated_at:   datetime
  accessed_at:  datetime     // Last time an agent retrieved this memory
  expires_at:   datetime?    // Hard expiration (computed from decay, or manual)
}
```

### 3.2 Memory Types

Typed memories let agents reason about *what kind* of knowledge they're retrieving.

| Type          | Description                                              | Example                                                        |
|---------------|----------------------------------------------------------|----------------------------------------------------------------|
| `decision`    | An architectural or design choice and its rationale      | "Chose Postgres over DynamoDB for ACID guarantees"             |
| `convention`  | A coding standard or pattern used in this project        | "All API responses use camelCase keys"                         |
| `hazard`      | A known pitfall, footgun, or dangerous pattern           | "Never call `sync()` outside a DB transaction"                 |
| `context`     | Background info that explains why something is the way it is | "This module was ported from Python, hence the naming style" |
| `intent`      | A planned change or temporary state                      | "Workaround for issue #432, replace with proper fix in v2.1"  |
| `relationship`| A dependency or interaction between components           | "PaymentService emits events consumed by NotificationService"  |
| `debug`       | A debugging insight or non-obvious behavior              | "The retry logic silently swallows 429s — check logs manually" |
| `preference`  | A human or team preference                               | "The team prefers explicit error types over string errors"     |

### 3.3 Decay Model

Decay models how a memory loses relevance over time. Not all memories decay — a `convention` may be permanent, while an `intent` becomes stale quickly.

```
Decay {
  strategy:     "linear" | "exponential" | "step" | "none"
  half_life:    duration?    // For exponential: time to reach 50% relevance
  ttl:          duration?    // For linear/step: hard time-to-live
  floor:        float        // Minimum relevance (default 0.0). A hazard might
                             // have floor=0.3 — it's always somewhat relevant.
}
```

**Effective relevance** at retrieval time:

```
relevance = criticality × decay_factor(age, decay_config)
```

Where `decay_factor` returns a value in `[decay.floor, 1.0]` based on the strategy. Memories below a configurable relevance threshold are excluded from retrieval (but not deleted — they can be explicitly queried).

**Default decay by type:**

| Type          | Default Decay                        |
|---------------|--------------------------------------|
| `decision`    | none                                 |
| `convention`  | none                                 |
| `hazard`      | none (floor: 0.5)                    |
| `context`     | none                                 |
| `intent`      | exponential, half_life: 14d          |
| `relationship`| none                                 |
| `debug`       | exponential, half_life: 30d          |
| `preference`  | none                                 |

### 3.4 Provenance

```
Provenance {
  source:       "agent" | "human" | "inferred" | "imported"
  agent_id:     string?      // Which agent instance created this
  model:        string?      // e.g. "claude-sonnet-4-5-20250929"
  session_id:   string?      // Trace back to the conversation
  reason:       string?      // Why was this memory created
}
```

Provenance affects trust-weighting: a human-authored `hazard` carries more weight than an agent-inferred one. This is exposed as a modifier on effective relevance:

```
trust_weight = {
  "human":    1.0,
  "agent":    0.85,
  "inferred": 0.6,
  "imported": 0.7
}
```

---

## 4. Scoping Model

### 4.1 Physical Scope

Physical scopes are file system paths relative to the project root. They support globs.

```
physical: ["src/api/auth/**"]           // Everything in the auth module
physical: ["src/api/auth/handlers.ts"]  // A specific file
physical: ["/"]                         // Project-wide
physical: ["src/api/**", "src/lib/http.ts"]  // Multiple paths
```

### 4.2 Logical Scope

Logical scopes are freeform labels that represent conceptual boundaries. They are hierarchical using dot notation.

```
logical: ["auth"]                        // The auth domain
logical: ["auth.oauth"]                  // Specifically OAuth within auth
logical: ["api-layer", "middleware"]      // Multiple logical scopes
logical: ["infrastructure.database"]     // Infra > DB
```

### 4.3 Scope Resolution (Retrieval)

When an agent is working in `src/api/auth/handlers.ts`, EngramDB resolves memories using a **bubbling** strategy:

1. Exact file match: `src/api/auth/handlers.ts`
2. Parent directory: `src/api/auth/**`
3. Grandparent: `src/api/**`
4. Project root: `/`
5. Logical scope matches (if the agent declares its current logical context)

Memories from narrower scopes rank higher. The final retrieval score combines scope_proximity with other factors (see §9.3 for full formula).

**Physical scope proximity (flat tiers):**

| Match Level         | Score | Example                          |
|---------------------|-------|----------------------------------|
| Exact file match    | 1.0   | `src/api/auth/handlers.ts`       |
| Same directory      | 0.85  | `src/api/auth/*`                 |
| Same module/parent  | 0.6   | `src/api/**`                     |
| Project root        | 0.4   | `/`                              |

**Logical scope bonus:** When a memory's logical scope matches the agent's declared logical context, an additive bonus of **+0.15 to +0.3** is applied to the scope_proximity score:

| Match Level          | Bonus |
|----------------------|-------|
| Exact match          | +0.3  |
| Parent scope match   | +0.2  |
| Sibling scope match  | +0.15 |

The bonus is additive but the total scope_proximity is capped at 1.0.

---

## 5. Storage Format

### 5.1 Directory Structure

The EngramDB splits across two locations: the **project directory** (shared, git-committed) and the **global config directory** (personal/generated, per-machine).

**Project directory (`.engramdb/` in project root):**
```
.engramdb/
├── manifest.toml          # Store metadata, schema version
├── config.toml            # Retrieval thresholds, decay defaults, scoring weights
├── index.json             # Lightweight index for fast lookups (machine-generated)
├── memories/
│   ├── <uuid>.md          # Individual memory files (frontmatter markdown)
│   └── ...
```

**Global config directory (`~/.config/engramdb/`):**
```
~/.config/engramdb/
├── registry.json          # Maps project IDs to paths and metadata
├── models/
│   └── all-MiniLM-L6-v2.onnx  # Shared ONNX model file (~23MB, auto-downloaded)
└── projects/
    └── <project-id>/
        ├── personal/
        │   ├── index.json         # Personal memory index
        │   └── memories/
        │       ├── <uuid>.md      # Personal memory files (frontmatter markdown)
        │       └── ...
        └── lancedb/
            └── memories.lance/    # LanceDB table (vectors + metadata index)
```

This separation keeps the repo clean — only shared, reviewable memories are committed. Embeddings (large, generated) and personal memories (private) live outside the repo entirely.

### 5.2 Project Identity

The MCP server is automatically scoped to the project via `cwd` (set in the agent host's MCP configuration). No explicit project hash is needed for basic operation.

For keying per-project data in `~/.config/engramdb/`, a **hybrid project ID** is computed:

```
1. If .git/config contains a remote "origin" URL:
   project_id = SHA-256(normalized_remote_url)[:16]

2. Otherwise:
   project_id = SHA-256(absolute_cwd_path)[:16]
```

**Normalization:** The remote URL is normalized before hashing — protocol stripped, `.git` suffix removed, lowercased. This ensures `https://github.com/user/repo.git` and `git@github.com:user/repo` produce the same ID.

**Benefits:**
- Git-based projects: the same repo cloned to different paths (or machines) shares a single project ID → personal memories and embeddings are reusable.
- Non-git projects: cwd path provides a stable identifier.
- 16-char hex prefix is short enough for directory names but collision-resistant enough for practical use.

**Registry (`~/.config/engramdb/registry.json`):**
```json
{
  "projects": {
    "a1b2c3d4e5f6g7h8": {
      "name": "my-app",
      "path": "/home/ege/projects/my-app",
      "remote": "github.com/ege/my-app",
      "last_accessed": "2026-02-10T14:00:00Z"
    }
  }
}
```

The registry is updated on every server start. It enables `engramdb list-projects` and helps recover from path changes.

### 5.3 Manifest (`manifest.toml`)

```toml
schema_version = "0.1.0"
project = "my-app"
created_at = 2026-02-10T12:00:00Z
description = "Agent EngramDB. See config.toml for retrieval settings."

[stats]
memory_count = 42
logical_scopes = ["auth", "auth.oauth", "payments", "api-layer", "infrastructure"]
```

### 5.4 Index (`index.json`)

The index is **machine-generated** — never hand-edited. It's rebuilt from memory files on `memory_reindex` or when inconsistencies are detected. It enables fast retrieval without parsing every markdown file.

```json
{
  "memories": [
    {
      "id": "019...",
      "type": "hazard",
      "summary": "Never call sync() outside a transaction",
      "physical": ["src/db/**"],
      "logical": ["infrastructure.database"],
      "tags": ["database", "transactions"],
      "criticality": 0.95,
      "confidence": 1.0,
      "provenance_source": "human",
      "created_at": "2026-01-15T10:00:00Z",
      "updated_at": "2026-01-15T10:00:00Z",
      "expires_at": null
    }
  ]
}
```

### 5.5 Individual Memory File (`<uuid>.md`)

Memories are stored as **frontmatter markdown** — YAML metadata in the frontmatter, content and details as markdown body with H2 section headers.

```markdown
---
id: "0195a3b7-8c4d-7e2f-a1b3-9d4e5f6a7b8c"
type: hazard
summary: "Never call sync() outside a transaction"
physical:
  - "src/db/**"
logical:
  - "infrastructure.database"
tags:
  - database
  - transactions
  - deadlock
  - incident
criticality: 0.95
confidence: 1.0
decay:
  strategy: none
  floor: 0.5
provenance:
  source: human
  reason: "Post-incident review of INC-2026-003"
status: active
visibility: shared
supersedes: []
challenges: []
created_at: "2026-01-15T10:00:00Z"
updated_at: "2026-01-15T10:00:00Z"
accessed_at: "2026-02-08T14:30:00Z"
expires_at: null
---

## Content

The `sync()` method in DatabaseClient acquires a write lock on the entire
connection pool. If called outside a transaction, it can deadlock with
concurrent reads.

This was the root cause of incident INC-2026-003. Always wrap in
`withTransaction()` first.

## Details

The incident occurred on 2026-01-14 at 03:42 UTC. The payment service
had 340 concurrent connections, and `sync()` was called from a background
job outside a transaction context.

### Timeline

- 03:42 — Background job triggered `sync()` without transaction
- 03:42 — Write lock acquired on connection pool
- 03:43 — 12 concurrent read queries blocked, connection pool exhausted
- 03:47 — Alert fired, on-call paged
- 03:51 — Service restarted, connections drained

### Fix applied

```rust
// Before (dangerous)
db.sync()?;

// After (safe)
db.with_transaction(|tx| {
    tx.sync()?;
    Ok(())
})?;
```

See PR #891 for the full fix.
```

**Parsing rules:**
- **Frontmatter** (between `---` delimiters): YAML, contains all structured metadata including `summary`.
- **`## Content`**: The core knowledge (~500 tokens). This is what gets embedded for semantic search and returned by default on `memory_retrieve`.
- **`## Details`**: Extended context (unlimited). Only loaded on `memory_get` or `memory_retrieve({ detail_level: "full" })`. Can contain sub-headings (H3+), code blocks, lists — any markdown.
- If `## Details` is absent, the memory has no details tier.
- The H2 headers `## Content` and `## Details` are reserved. Other H2 headers are not allowed at the top level.

### 5.5 Personal Memories

Some memories are developer-specific or agent-specific and shouldn't be shared with the team. These live in the **global config directory**, not the project repo:

```
~/.config/engramdb/projects/<project-id>/personal/
├── index.json             # Personal memory index
└── memories/
    └── <uuid>.json        # Personal memory files
```

This keeps the repo directory completely clean — no `.gitignore` entries needed for personal data. Personal memories are keyed by project ID (see §5.2), so they survive directory moves for git-based projects.

Personal memories participate in the same retrieval pipeline — they're merged with shared memories at query time and scored identically. The `Memory` schema includes:

```
visibility:   "shared" | "personal"    // default: "shared"
```

**Use cases for personal memories:**
- Agent-specific session context ("I'm in the middle of refactoring X")
- Developer preferences that differ from team conventions
- Draft memories not yet ready for team review
- Environment-specific notes ("my local DB is on port 5433")
- Sensitive context (credentials locations, personal workflow notes)

**Promotion:** A personal memory can be promoted to shared via `memory.update(id, { visibility: "shared" })`, which moves the file from the global config directory into the project's `.engramdb/memories/` and updates both indexes.

### 5.7 Config (`config.toml`)

```toml
[retrieval]
relevance_threshold = 0.3
max_results = 10
include_expired = false

[retrieval.scoring.with_query]
semantic = 0.55
relevance = 0.45

[retrieval.scoring.with_keyword]
keyword = 0.45
semantic = 0.30
relevance = 0.25

[retrieval.scoring.scope_only]
relevance = 1.0

[retrieval.scoring.degraded]
relevance = 1.0

scope_multiplier_floor = 0.5
trust_multiplier_floor = 0.5
challenge_penalty = 0.10

[scope_proximity]
exact_file = 1.0
same_directory = 0.85
same_module = 0.6
project_root = 0.4

[scope_proximity.logical_bonus]
exact = 0.3
parent = 0.2
sibling = 0.15

[decay_defaults.intent]
strategy = "exponential"
half_life = "14d"

[decay_defaults.debug]
strategy = "exponential"
half_life = "30d"

[trust_weights]
human = 1.0
agent = 0.85
inferred = 0.6
imported = 0.7

[thresholds]
needs_review = 0.3
gc = 0.05
compress = 0.4

[embeddings]
enabled = true
provider = "onnx"
model = "all-MiniLM-L6-v2"
dimensions = 384

[auto_gc]
enabled = true
run_on_retrieval = false
```

### 5.7 Complete Defaults Reference

All configurable values in one place. Every value is overridable in `config.toml`.

#### Scoring Weights

| Context                           | Semantic | Relevance | Scope | Trust |
|-----------------------------------|----------|-----------|-------|-------|
| With text query + embeddings      | 0.3      | 0.4       | 0.2   | 0.1   |
| With text query, no embeddings    | —        | 0.6       | 0.25  | 0.15  |
| Scope-only retrieval (no query)   | —        | 0.4       | 0.4   | 0.2   |

#### Physical Scope Proximity

| Match Level      | Score |
|------------------|-------|
| Exact file       | 1.0   |
| Same directory   | 0.85  |
| Same module      | 0.6   |
| Project root     | 0.4   |

#### Logical Scope Bonus (additive, capped at 1.0)

| Match Level    | Bonus |
|----------------|-------|
| Exact match    | +0.3  |
| Parent scope   | +0.2  |
| Sibling scope  | +0.15 |

#### Trust Weights

| Provenance Source | Weight |
|-------------------|--------|
| `human`           | 1.0    |
| `agent`           | 0.85   |
| `imported`        | 0.7    |
| `inferred`        | 0.6    |

#### Thresholds

| Threshold          | Value | Effect                                           |
|--------------------|-------|--------------------------------------------------|
| `relevance_threshold` | 0.3  | Memories below this excluded from retrieval    |
| `needs_review`     | 0.3   | Decayed memories auto-flagged for review         |
| `compress`         | 0.4   | Memories below this eligible for compression     |
| `gc`               | 0.05  | Memories below this eligible for garbage collection |

#### Decay Defaults by Type

| Type          | Strategy     | Half-life | Floor | Notes                     |
|---------------|-------------|-----------|-------|---------------------------|
| `decision`    | none         | —         | —     | Permanent                 |
| `convention`  | none         | —         | —     | Permanent                 |
| `hazard`      | none         | —         | 0.5   | Permanent, min 50% relevance |
| `context`     | none         | —         | —     | Permanent                 |
| `intent`      | exponential  | 14 days   | 0.0   | Fades quickly             |
| `relationship`| none         | —         | —     | Permanent                 |
| `debug`       | exponential  | 30 days   | 0.0   | Fades over a month        |
| `preference`  | none         | —         | —     | Permanent                 |

#### Retrieval Defaults

| Parameter        | Value |
|------------------|-------|
| `max_results`    | 10    |
| `include_expired`| false |

#### Challenge

| Parameter           | Value |
|---------------------|-------|
| `relevance_penalty` | 0.3 (30% reduction to final score) |
```

---

## 6. API Surface

The EngramDB exposes a uniform API usable via MCP, CLI, or direct library import.

### 6.1 Commands

#### `memory.create`

```
Input:
  type:         MemoryType (required)
  content:      string (required)
  summary:      string (optional — auto-generated if omitted)
  physical:     string[] (default: ["/"])
  logical:      string[] (default: [])
  tags:         string[] (default: [])
  criticality:  float (default: 0.5)
  decay:        Decay? (default: type-based default)
  confidence:   float (default: 0.8)
  supersedes:   string[] (default: [])

Output:
  id:           string
  created:      boolean
```

#### `memory.retrieve`

The primary read operation. Returns memories relevant to the current context.

```
Input:
  path:         string?       // Current file path (for physical scope resolution)
  logical:      string[]?     // Current logical context
  types:        MemoryType[]? // Filter by type
  tags:         string[]?     // Filter by tags (OR)
  min_criticality: float?     // Floor filter
  max_results:  int?          // Override config default
  include_expired: bool?      // Include decayed/expired memories

Output:
  memories:     Memory[]      // Sorted by score descending
  total:        int           // Total matching (before max_results)
```

#### `memory.get`

```
Input:
  id:           string

Output:
  memory:       Memory
```

#### `memory.update`

```
Input:
  id:           string (required)
  [any Memory field except id, created_at]

Output:
  updated:      boolean
```

#### `memory.delete`

```
Input:
  id:           string

Output:
  deleted:      boolean
```

#### `memory.search`

Full-text search across content and summaries.

```
Input:
  query:        string
  filters:      {  // All optional
    types:      MemoryType[]
    tags:       string[]
    physical:   string
    logical:    string
    min_criticality: float
  }

Output:
  memories:     Memory[]
```

#### `memory.gc`

Garbage collection — removes memories below relevance threshold.

```
Input:
  dry_run:      bool (default: true)
  threshold:    float? (override config)

Output:
  removed:      string[]      // IDs of removed memories
  count:        int
```

#### `memory.stats`

```
Output:
  total:        int
  by_type:      { [MemoryType]: int }
  by_scope:     { [logical_scope]: int }
  by_status:    { active: int, needs_review: int, challenged: int }
  expired:      int
  avg_criticality: float
  oldest:       datetime
  newest:       datetime
```

#### `memory.challenge`

Flag a memory as contradicted by new evidence. Sets status to `"challenged"` and reduces effective relevance. The agent should surface the conflict to the user for resolution.

```
Input:
  id:           string (required)
  evidence:     string (required)  // What contradicts this memory
  source_file:  string?            // Where the contradiction was found

Output:
  challenged:   boolean
  memory:       Memory             // Updated memory with challenge appended
```

#### `memory.review`

Returns all memories needing human attention (status: `needs_review` or `challenged`), ordered by criticality. Designed for interactive resolution.

```
Input:
  scope:        string?            // Filter to a logical or physical scope
  max_results:  int?               // Default: all

Output:
  memories:     Memory[]           // With challenges and decay info included
  total:        int
```

#### `memory.resolve`

Resolve a challenged or needs_review memory after human decision.

```
Input:
  id:           string (required)
  action:       "keep" | "update" | "delete" (required)
  updated_content: string?         // Required if action is "update"
  updated_summary: string?

Output:
  resolved:     boolean
```

#### `memory.compress`

Summarize a group of low-relevance memories into a single distilled memory. Source memories are archived, not deleted.

```
Input:
  scope:        string?            // Logical or physical scope to compress
  threshold:    float?             // Max relevance to include (default: 0.4)
  dry_run:      bool (default: true)

Output:
  source_ids:   string[]           // Memories that would be / were compressed
  summary:      Memory?            // The generated summary memory (null if dry_run)
  archived:     int                // Count of archived source memories
```

#### `memory.reindex`

Regenerate the search index and embedding vectors. Needed after clone, model change, or manual file edits.

```
Input:
  embeddings_only: bool (default: false)  // Skip index rebuild, just re-embed

Output:
  indexed:      int
  embedded:     int
  errors:       string[]
```

#### `memory.export` / `memory.import`

```
// Export
Input:
  filters:      { types?, tags?, physical?, logical? }  // Optional filters
Output:
  bundle:       JSON               // Portable memory bundle

// Import
Input:
  bundle:       JSON (required)
  scope_remap:  { [old_path]: new_path }?  // Remap physical scopes
  conflict:     "skip" | "overwrite" (default: "skip")
Output:
  imported:     int
  skipped:      int
```

---

## 7. Discoverability

How agents learn that EngramDB exists and how to use it.

### 7.1 MCP Server (Primary)

The EngramDB runs as an MCP server. Agents with MCP support automatically discover available tools.

```json
// MCP tool registration
{
  "name": "engramdb",
  "description": "Project-scoped EngramDB. Retrieve relevant memories before making changes. Store important decisions, hazards, and conventions.",
  "tools": ["memory.create", "memory.retrieve", "memory.search", "memory.update", "memory.delete", "memory.gc", "memory.stats"]
}
```

The MCP server can also use **proactive injection**: when it detects an agent opening or editing a file (via resource subscriptions), it can surface relevant memories without being asked.

### 7.2 Convention File Bootstrap

For agents that don't support MCP or as a fallback:

```markdown
<!-- In CLAUDE.md, AGENTS.md, or similar -->
## EngramDB Store

This project uses `.engramdb/` for persistent knowledge.
Before modifying files, check for relevant memories:
  - Review `.engramdb/index.json` for applicable entries
  - Load full memory from `.engramdb/memories/<id>.json` when relevant
After making significant discoveries or decisions, create a memory.
```

### 7.3 CLI

```bash
# Initialize in a project
engramdb init

# Quick add
engramdb add --type hazard --content "..." --physical "src/db/**" --criticality 0.9

# Retrieve for current context
engramdb retrieve --path src/api/auth/handlers.ts

# Search
engramdb search "transaction deadlock"

# Garbage collect
engramdb gc --dry-run

# Stats
engramdb stats
```

### 7.4 Git Integration

- `.engramdb/` is committed to the repo, so memories are shared across the team.
- `.engramdb/memories/*.json` files can be reviewed in PRs.
- A `.gitattributes` entry can mark index.json as generated (merge=ours or similar).

---

## 8. Agent Interaction Patterns

### 8.1 On Session Start

```
1. Agent detects .engramdb/ exists (via MCP or convention file)
2. Agent calls memory.retrieve(path: current_file) or memory.stats()
3. High-criticality memories are injected into context
```

### 8.2 During Work

```
1. Agent opens/reads a file
   → memory.retrieve(path: "src/api/auth/handlers.ts")
   → Surfaces: "OAuth tokens must be validated server-side (criticality: 0.9)"

2. Agent encounters a bug
   → memory.search("null pointer auth middleware")
   → Finds: debug memory about a known race condition

3. Agent makes an architectural decision
   → memory.create(type: "decision", content: "Switched to JWT...", ...)
```

### 8.3 On Session End (Optional)

```
1. Agent reviews significant actions taken during session
2. Agent creates memories for important discoveries, decisions, or hazards
3. memory.gc(dry_run: false) — clean up expired entries
```

---

## 9. Resolved Design Decisions

### 9.1 Conflict Resolution → Last-Write-Wins

The EngramDB uses **last-write-wins** semantics. No application-level conflict resolution.

**Rationale:** The store is file-based and git-committed. Git already provides robust conflict resolution for the team-level case (two developers' agents writing conflicting memories). At the agent level, conflicts are rare — and when they happen, the most recent information is usually the most accurate.

- On `memory.update`, the file is simply overwritten with the new content and `updated_at` is set.
- If two agents race on the same memory, the last write persists. The `supersedes` field can be used to explicitly mark one memory as replacing another.
- For shared memories, git merge conflicts on individual `.json` files are resolved through normal git workflows.
- The `provenance` field preserves history of who wrote what, so conflicts can be audited after the fact.

### 9.2 Memory Size → Tiered (Summary + Content + Details)

Memory content is split into three tiers with different loading strategies:

| Tier       | Max Size    | Loaded When            | Present In     |
|------------|-------------|------------------------|----------------|
| `summary`  | 100 chars   | Always (index scan)    | `index.json`   |
| `content`  | ~500 tokens | On retrieval           | `<id>.json`    |
| `details`  | Unlimited   | On explicit request    | `<id>.json`    |

**Retrieval behavior:**
- `memory.retrieve` returns `summary` + `content` by default.
- `memory.retrieve({ detail_level: "summary" })` returns only summaries (cheap, for scanning).
- `memory.get(id)` returns everything including `details`.
- The `details` field is optional. Most memories won't need it. It's for cases like incident post-mortems, long rationales, or embedded code samples.

**Validation on create/update:**
- `summary` is required and must be ≤100 characters. If omitted, auto-generated from `content`.
- `content` exceeding ~500 tokens triggers a warning (not a hard error) suggesting the author move overflow to `details`.

### 9.3 Semantic Search → LanceDB + ONNX (Optional Add-on)

The EngramDB includes **embedding-based semantic search** as an optional but high-value add-on. When enabled, it significantly improves retrieval quality by matching on meaning rather than just keywords.

#### Architecture

The semantic search stack is fully embedded in the Rust MCP server binary — no external services required:

- **Embedding model:** `all-MiniLM-L6-v2` via ONNX Runtime (`ort` crate). The model is ~23MB and runs CPU inference in sub-millisecond per embedding. Bundled with the binary or downloaded on first run.
- **Vector storage:** LanceDB (`lancedb` crate). Embedded columnar database with native vector search (IVF-PQ indexing). No separate server process.
- **Combined queries:** LanceDB supports metadata filtering alongside vector search in a single query — filter by scope/tags/type, then rank by semantic similarity.

**Storage location (in global config, not repo):**
```
~/.config/engramdb/projects/<project-id>/
├── lancedb/               # LanceDB data files (vector index + metadata)
│   └── memories.lance/    # Lance table with embeddings and memory references
└── models/
    └── all-MiniLM-L6-v2.onnx   # ONNX model file (shared across projects)
```

Note: The ONNX model file can be shared across projects. It may live at `~/.config/engramdb/models/` at the top level rather than per-project.

#### Embedding Pipeline

```
On memory_create / memory_update:
  1. Concatenate summary + " " + content (not details — too large, too noisy)
  2. Tokenize and run through ONNX MiniLM → 384-dimensional float32 vector
  3. Upsert into LanceDB table: { id, vector, physical, logical, tags, type, criticality, ... }

On memory_retrieve / memory_search (with text query):
  1. Embed the query text → 384-dimensional vector
  2. LanceDB query:
     - Pre-filter: physical scope glob, logical scope, type, tags, expiration
     - Vector search: cosine similarity against stored embeddings
     - Return top N candidates with similarity scores
  3. Merge LanceDB semantic_score into the composite scoring formula
```

#### Configurable Embedding Model

The default is `all-MiniLM-L6-v2` (384 dimensions, ~23MB, excellent quality-to-size ratio). The model is **auto-downloaded on first server start** if not already present at `~/.config/engramdb/models/`. The model is configurable in `config.toml`:

```toml
[embeddings]
enabled = true
provider = "onnx"
model = "all-MiniLM-L6-v2"
dimensions = 384
# model_path = "/custom/path/to/model.onnx"  # Override auto-download location
```

Supported providers (v1: `onnx` only, extensible later):

| Provider | Config value | Notes |
|----------|-------------|-------|
| ONNX Runtime (default) | `"onnx"` | Bundled, offline, zero config |
| Ollama (future) | `"ollama"` | For users who want larger/better models locally |
| API (future) | `"api"` | OpenAI, Voyage, etc. for best quality |

When switching models, `memory_reindex` must be run to regenerate all vectors (dimensions may differ).

#### Graceful Degradation

When embeddings are **not available** (ONNX model missing, LanceDB initialization fails, or `embeddings.enabled: false`):

1. The system operates in **keyword + tag + scope scoring mode** — fully functional for `memory_retrieve` (scope-based) and partially functional for `memory_search` (keyword matching on summary/content/tags).
2. A warning is surfaced on server start and on the first `memory_search` call:
   ```
   ⚠ Semantic search unavailable — using keyword matching.
   Run 'engramdb setup-embeddings' to enable semantic search for better retrieval quality.
   ```
3. The scoring formula adapts: when `semantic_score` is unavailable, the remaining weights are renormalized to the active components only.

**Retrieval pipeline:**
```
1. Query comes in (path, logical scope, optional text query)
2. Candidate filtering:
   - Physical scope match (glob)
   - Logical scope match
   - Type/tag filters
   - Expiration check
3. Scoring (for each candidate):
   a. base_score    = weighted sum of active signals (semantic, keyword, relevance)
   b. scope_mult    = floor + (1 - floor) * scope_score  (or 1.0 if no scope context)
   c. trust_mult    = floor + (1 - floor) * trust_weight
   d. score         = base_score * scope_mult * trust_mult
   e. If challenged: score -= challenge_penalty (default 0.10)
   f. score = clamp(score, 0, 1)

4. Modes (determine which weights for base_score):
   WITH keyword search:         0.45*keyword + 0.30*semantic + 0.25*relevance
   WITH query + embeddings:     0.55*semantic + 0.45*relevance
   WITH query, NO embeddings:   keyword search on loaded memories (degraded_keyword),
                                or 1.0*relevance if no keyword matches (degraded)
   WITHOUT query (scope-only):  1.0*relevance

5. Filter out scores below relevance_threshold (default: 0.3)
6. Sort by score descending, return top N (default: 10)
```

All weights and thresholds are configurable in `config.toml`.

---

### 9.4 Cross-Project Memories → Project-Scoped Only

The EngramDB is **strictly project-scoped**. There is no global `~/.engramdb/` store.

**Rationale:** This tool exists specifically for project-specific knowledge — the kind of context that's lost between agent sessions on a particular codebase. Global/personal preferences already have established mechanisms (CLAUDE.md in home directory, agent configuration files, user preferences). Mixing global and local memories adds retrieval complexity and scope ambiguity.

**For reuse across projects:**
- `engramdb export` — export memories (or a filtered subset) as a portable JSON bundle.
- `engramdb import <bundle.json>` — import memories into the current project, with optional remapping of physical scopes.
- `engramdb init --template <name>` — initialize with a predefined set of convention/preference memories (e.g., a team template).

Templates are plain JSON bundles stored anywhere (a shared repo, a gist, a local file). They are not a special mechanism — just the import format with a name.

### 9.5 Memory Validation → Hybrid (Decay + Agent Challenge + Interactive Review)

Stale memory detection uses three complementary mechanisms:

#### A. Passive Decay (Automatic)

Memories with a decay configuration naturally lose relevance over time (see §3.3). When a memory's effective relevance drops below the `needs_review_threshold` (default: 0.3) but remains above the GC threshold (default: 0.05), it is automatically flagged as `status: "needs_review"`.

#### B. Agent Challenge Protocol (Active)

When an agent encounters evidence that contradicts an existing memory, it **must not silently override it**. Instead:

1. Agent calls `memory.challenge(id, evidence: "...")` — creates a challenge record on the memory.
2. The memory gains a `challenges` array:
   ```json
   {
     "challenges": [
       {
         "timestamp": "2026-02-10T14:00:00Z",
         "agent_id": "claude-session-abc",
         "evidence": "The API now returns camelCase keys, contradicting this convention memory.",
         "source_file": "src/api/response.ts"
       }
     ]
   }
   ```
3. The memory's status is set to `"challenged"` and its effective relevance is reduced by a flat penalty (default 0.10 subtracted from final score) until resolved.
4. **The agent surfaces the conflict to the user** rather than making a unilateral decision. The agent should present both the existing memory and the contradicting evidence, and ask the user how to resolve it.

#### C. Interactive Review Command (Human-in-the-Loop)

```bash
engramdb review
```

Surfaces all memories with `status: "needs_review"` or `status: "challenged"`, ordered by criticality. For each memory:

```
┌─────────────────────────────────────────────────────┐
│ [hazard] Never call sync() outside a transaction    │
│ Criticality: 0.95  │  Relevance: 0.28 (decayed)    │
│ Created: 2025-08-12  │  Last verified: 2025-11-03   │
│                                                     │
│ ⚠ Challenged by agent on 2026-02-10:               │
│   "sync() now uses row-level locks as of PR #891,   │
│    the deadlock risk no longer applies."             │
│                                                     │
│ [k]eep  [u]pdate  [d]elete  [s]kip                 │
└─────────────────────────────────────────────────────┘
```

- **Keep**: resets decay clock, sets `verified_at: now`, clears challenges, status → `"active"`.
- **Update**: opens the memory for editing, then keeps.
- **Delete**: removes the memory.
- **Skip**: leaves it for next review.

The review command can also be invoked programmatically via MCP (`memory.review`) for agents that want to conduct review with the user in-chat.

**New fields on Memory:**
```
status:        "active" | "needs_review" | "challenged"  // default: "active"
verified_at:   datetime?     // Last time a human confirmed accuracy
challenges:    Challenge[]   // Pending contradictions
```

### 9.6 Auto-Summarization → Decay-Weighted Compression (Manual Trigger)

The store provides a `memory.compress` command that summarizes groups of related memories into higher-level memories. It is **never automatic** — always explicitly triggered.

```bash
engramdb compress --scope "auth" --dry-run
```

**How compression works:**

1. Select candidate memories: those within the target scope that have decayed below a threshold (default: `relevance < 0.4`) or have `status: "needs_review"`.
2. **Decay-weighted inclusion**: memories with higher remaining relevance contribute more to the summary. Near-zero relevance memories are mentioned briefly or omitted entirely.
3. An agent (or the configured LLM) generates a summary memory that captures the essential knowledge from the group.
4. The summary memory:
   - Has `type: "context"` (or a new `type: "summary"`)
   - Has `provenance.source: "inferred"`
   - Lists all source memory IDs in a `compressed_from: string[]` field
   - Inherits the highest criticality from its source memories
   - Has `decay.strategy: "none"` (summaries don't decay — they're already distilled)
5. Source memories are **archived** (moved to `.engramdb/archive/`), not deleted. They can be restored if the summary loses nuance.

**Example output:**
```
Compressing 7 memories in scope "auth" (relevance range: 0.08–0.35):

Summary: "The auth module was migrated from Python in 2025. It uses JWT
with RS256, validates tokens server-side only, and has had 3 incidents
related to token refresh race conditions (all resolved). OAuth scopes
are defined in auth/scopes.ts and must match the API gateway config."

[c]onfirm  [e]dit  [a]bort
```

### 9.7 Permissions → Shared + Personal (v1)

For v1, visibility is binary: `shared` or `personal` (see §5.5). No team-scoping, RBAC, or audience labels.

This is a deliberate simplification. The file-based storage model means that more granular permissions would require either filesystem-level ACLs (fragile) or an application-level auth layer (overengineered for v1). If enterprise use cases emerge, permissions can be layered on in v2 via a server mode.

### 9.8 All Defaults and Scoring Values

See §5.7 for the complete reference table of all configurable values, including scoring weights, scope proximity tiers, trust weights, thresholds, and decay defaults. All values are overridable in `config.toml`.

---

## 11. MCP Server Contract

The EngramDB's primary integration point is as an MCP (Model Context Protocol) server. This section defines the complete contract: server metadata, tools, resources, prompts, and interaction patterns.

### 11.1 Server Metadata

```json
{
  "name": "engramdb",
  "version": "0.1.0",
  "description": "Project-scoped persistent EngramDB for coding agents. Stores decisions, hazards, conventions, and context about the codebase. Retrieve before modifying files. Store after significant discoveries.",
  "capabilities": {
    "tools": true,
    "resources": true,
    "prompts": true
  }
}
```

### 11.2 Tools

All tools follow the MCP tool schema. Input validation errors return structured error responses, not exceptions.

---

#### `memory_create`

Create a new memory.

```json
{
  "name": "memory_create",
  "description": "Store a new memory about the project. Use after discovering important patterns, making architectural decisions, encountering hazards, or learning conventions. The memory will be scoped to the specified paths and domains, and retrievable by any agent working in those areas.",
  "inputSchema": {
    "type": "object",
    "required": ["type", "content"],
    "properties": {
      "type": {
        "type": "string",
        "enum": ["decision", "convention", "hazard", "context", "intent", "relationship", "debug", "preference"],
        "description": "The kind of knowledge. 'hazard' for footguns/pitfalls, 'decision' for architectural choices with rationale, 'convention' for coding standards, 'intent' for planned/temporary changes, 'debug' for non-obvious behaviors, 'context' for background explanations, 'relationship' for component dependencies, 'preference' for team/human preferences."
      },
      "content": {
        "type": "string",
        "description": "The core knowledge to store. Should be self-contained and actionable. Max ~500 tokens — move extended context to 'details'."
      },
      "summary": {
        "type": "string",
        "maxLength": 100,
        "description": "One-line summary, max 100 chars. Auto-generated from content if omitted."
      },
      "details": {
        "type": "string",
        "description": "Extended context, code samples, incident reports, or rationale. Lazy-loaded — only fetched on explicit memory_get. No size limit."
      },
      "physical": {
        "type": "array",
        "items": { "type": "string" },
        "default": ["/"],
        "description": "File/folder paths this memory applies to, relative to project root. Supports globs (e.g., 'src/api/auth/**'). Default: project-wide."
      },
      "logical": {
        "type": "array",
        "items": { "type": "string" },
        "default": [],
        "description": "Logical scopes using dot notation (e.g., 'auth.oauth', 'infrastructure.database'). Used for cross-cutting concerns that span multiple directories."
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" },
        "default": [],
        "description": "Freeform tags for flexible querying."
      },
      "criticality": {
        "type": "number",
        "minimum": 0.0,
        "maximum": 1.0,
        "default": 0.5,
        "description": "How important this memory is. 0.0 = trivial, 1.0 = critical/must-know. Affects retrieval ranking and decay behavior."
      },
      "confidence": {
        "type": "number",
        "minimum": 0.0,
        "maximum": 1.0,
        "default": 0.8,
        "description": "How certain you are about this information. 0.0 = speculation, 1.0 = verified fact."
      },
      "decay_strategy": { "type": "string", "enum": ["linear", "exponential", "step", "none"], "description": "Override the default decay strategy. Omit to use type-based defaults." },
      "decay_half_life": { "type": "integer", "description": "Half-life in seconds for exponential decay." },
      "decay_ttl": { "type": "integer", "description": "Time-to-live in seconds for linear/step decay." },
      "decay_floor": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Minimum decay factor." },
      "supersedes": {
        "type": "array",
        "items": { "type": "string" },
        "default": [],
        "description": "IDs of memories this replaces. Superseded memories are marked as such but not deleted."
      },
      "visibility": {
        "type": "string",
        "enum": ["shared", "personal"],
        "default": "shared",
        "description": "'shared' memories are committed to git. 'personal' memories are gitignored and local to this developer/agent."
      }
    }
  }
}
```

> **Design note:** Decay fields are flat (`decay_strategy`, `decay_half_life`, etc.) rather than nested under a `decay` object. This is intentional for LLM tool-calling ergonomics — flat schemas are easier for models to populate correctly.

**Response:**
```json
{
  "id": "019...",
  "created": true,
  "summary": "Never call sync() outside a transaction"
}
```

---

#### `memory_retrieve`

Retrieve memories relevant to the current working context. This is the primary read operation — agents should call it when opening or modifying files.

```json
{
  "name": "memory_retrieve",
  "description": "Get memories relevant to your current working context. Call this before modifying files to surface decisions, hazards, and conventions that apply. Returns memories sorted by relevance score.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "path": {
        "type": "string",
        "description": "Current file path relative to project root (e.g., 'src/api/auth/handlers.ts'). Memories scoped to this file and parent directories will be retrieved with proximity-based scoring."
      },
      "logical": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Current logical context (e.g., ['auth', 'api-layer']). Adds scoring bonus for matching memories."
      },
      "query": {
        "type": "string",
        "description": "Optional text query for semantic search. When provided, scoring includes embedding similarity. When omitted, retrieval is purely scope-based."
      },
      "types": {
        "type": "array",
        "items": { "type": "string", "enum": ["decision", "convention", "hazard", "context", "intent", "relationship", "debug", "preference"] },
        "description": "Filter to specific memory types."
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Filter by tags (OR logic — matches any tag)."
      },
      "min_criticality": {
        "type": "number",
        "minimum": 0.0,
        "maximum": 1.0,
        "description": "Only return memories with criticality >= this value."
      },
      "max_results": {
        "type": "integer",
        "default": 10,
        "description": "Maximum number of memories to return."
      },
      "detail_level": {
        "type": "string",
        "enum": ["summary", "content", "full"],
        "default": "content",
        "description": "'summary' = summaries only (cheapest). 'content' = summary + content (default). 'full' = everything including details (expensive)."
      },
      "include_expired": {
        "type": "boolean",
        "default": false,
        "description": "Include memories that have fully decayed or expired."
      }
    }
  }
}
```

**Response:**
```json
{
  "memories": [
    {
      "id": "019...",
      "type": "hazard",
      "summary": "Never call sync() outside a transaction",
      "content": "The sync() method acquires a write lock on the entire connection pool...",
      "physical": ["src/db/**"],
      "logical": ["infrastructure.database"],
      "tags": ["database", "transactions"],
      "criticality": 0.95,
      "confidence": 1.0,
      "status": "active",
      "score": 0.87,
      "score_breakdown": {
        "semantic": null,
        "relevance": 0.95,
        "scope": 0.85,
        "trust": 1.0
      }
    }
  ],
  "total": 3,
  "query_mode": "scope_only"
}
```

---

#### `memory_search`

Full-text and semantic search across all memories.

```json
{
  "name": "memory_search",
  "description": "Search across all memories by text content. Use when you need to find specific knowledge regardless of your current file context — e.g., 'how do we handle authentication?' or 'transaction deadlock'.",
  "inputSchema": {
    "type": "object",
    "required": ["query"],
    "properties": {
      "query": {
        "type": "string",
        "description": "Search query. Matched against summary, content, and tags using both keyword and semantic similarity."
      },
      "types": {
        "type": "array",
        "items": { "type": "string" }
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" }
      },
      "physical": {
        "type": "string",
        "description": "Filter to memories scoped to this path."
      },
      "logical": {
        "type": "string",
        "description": "Filter to memories in this logical scope."
      },
      "min_criticality": {
        "type": "number"
      },
      "max_results": {
        "type": "integer",
        "default": 10
      }
    }
  }
}
```

---

#### `memory_get`

Fetch a single memory with full details.

```json
{
  "name": "memory_get",
  "description": "Get the full content of a specific memory, including the 'details' field. Use when a retrieved memory's summary/content indicates there's more context you need.",
  "inputSchema": {
    "type": "object",
    "required": ["id"],
    "properties": {
      "id": { "type": "string" }
    }
  }
}
```

---

#### `memory_update`

```json
{
  "name": "memory_update",
  "description": "Update an existing memory. Use to correct inaccuracies, add context, change scope, or adjust criticality. Any field can be updated except 'id' and 'created_at'.",
  "inputSchema": {
    "type": "object",
    "required": ["id"],
    "properties": {
      "id": { "type": "string" },
      "type": { "type": "string" },
      "content": { "type": "string" },
      "summary": { "type": "string" },
      "details": { "type": "string" },
      "physical": { "type": "array", "items": { "type": "string" } },
      "logical": { "type": "array", "items": { "type": "string" } },
      "tags": { "type": "array", "items": { "type": "string" } },
      "criticality": { "type": "number" },
      "confidence": { "type": "number" },
      "decay_strategy": { "type": "string", "enum": ["linear", "exponential", "step", "none"], "description": "Override the decay strategy." },
      "decay_half_life": { "type": "integer", "description": "Half-life in seconds for exponential decay." },
      "decay_ttl": { "type": "integer", "description": "Time-to-live in seconds for linear/step decay." },
      "decay_floor": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Minimum decay factor." },
      "supersedes": { "type": "array", "items": { "type": "string" } },
      "visibility": { "type": "string", "enum": ["shared", "personal"] }
    }
  }
}
```

---

#### `memory_delete`

```json
{
  "name": "memory_delete",
  "description": "Permanently delete a memory. Prefer memory_update with supersedes for corrections, or let decay/GC handle natural expiration. Use delete for genuinely wrong or harmful memories.",
  "inputSchema": {
    "type": "object",
    "required": ["id"],
    "properties": {
      "id": { "type": "string" }
    }
  }
}
```

---

#### `memory_challenge`

```json
{
  "name": "memory_challenge",
  "description": "Flag a memory as potentially incorrect based on new evidence. This reduces its retrieval score by 30% and marks it for human review. IMPORTANT: After challenging, surface the conflict to the user and ask how to resolve it — do not silently override.",
  "inputSchema": {
    "type": "object",
    "required": ["id", "evidence"],
    "properties": {
      "id": { "type": "string" },
      "evidence": {
        "type": "string",
        "description": "What you observed that contradicts this memory."
      },
      "source_file": {
        "type": "string",
        "description": "File where the contradicting evidence was found."
      }
    }
  }
}
```

---

#### `memory_review`

```json
{
  "name": "memory_review",
  "description": "List memories that need human attention — either auto-flagged as stale (needs_review) or challenged by an agent. Present these to the user for resolution.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "scope": { "type": "string", "description": "Filter to a logical or physical scope." },
      "max_results": { "type": "integer", "default": 10 }
    }
  }
}
```

---

#### `memory_resolve`

```json
{
  "name": "memory_resolve",
  "description": "Resolve a challenged or needs_review memory after human decision. Use 'keep' to confirm accuracy (resets decay, clears challenges), 'update' to correct it, or 'delete' to remove it.",
  "inputSchema": {
    "type": "object",
    "required": ["id", "action"],
    "properties": {
      "id": { "type": "string" },
      "action": { "type": "string", "enum": ["keep", "update", "delete"] },
      "updated_content": { "type": "string", "description": "New content (required when action is 'update')." },
      "updated_summary": { "type": "string", "description": "New summary (optional when action is 'update')." }
    }
  }
}
```

---

#### `memory_compress_candidates`

List memories eligible for compression. Review candidates before calling `memory_compress_apply`.

```json
{
  "name": "memory_compress_candidates",
  "description": "List memories eligible for compression — those with criticality at or below the threshold. Review candidates before calling memory_compress_apply.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "scope": { "type": "string", "description": "Logical or physical scope to filter candidates." },
      "threshold": { "type": "number", "default": 0.4, "description": "Criticality threshold. Memories at or below this value are candidates." }
    }
  }
}
```

**Response:**
```json
{
  "candidates": [
    { "id": "019...", "type": "debug", "summary": "...", "criticality": 0.1 }
  ],
  "total": 3,
  "threshold": 0.4
}
```

---

#### `memory_compress_apply`

Compress multiple memories into a single summary memory. The agent provides the summary and content; the system creates the new memory (type `context`, provenance `agent:compress`) and records `supersedes` on the new memory automatically.

```json
{
  "name": "memory_compress_apply",
  "description": "Compress multiple memories into a single summary memory. You provide the summary and content; the system creates the new memory and marks source memories as superseded. Always call memory_compress_candidates first.",
  "inputSchema": {
    "type": "object",
    "required": ["source_ids", "summary", "content"],
    "properties": {
      "source_ids": { "type": "array", "items": { "type": "string" }, "description": "IDs of memories to compress." },
      "summary": { "type": "string", "description": "One-line summary of the compressed memory." },
      "content": { "type": "string", "description": "Full content of the compressed memory." },
      "scope": { "type": "array", "items": { "type": "string" }, "description": "Logical scopes for the new memory." },
      "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags for the new memory." }
    }
  }
}
```

**Response:**
```json
{
  "new_id": "019...",
  "superseded_count": 3,
  "applied": true
}
```

---

#### `memory_stats`

```json
{
  "name": "memory_stats",
  "description": "Get an overview of EngramDB — counts by type, scope, status, and health indicators.",
  "inputSchema": {
    "type": "object",
    "properties": {}
  }
}
```

---

#### `memory_gc`

```json
{
  "name": "memory_gc",
  "description": "Garbage collect memories that have decayed below the GC threshold (default 0.05). Always dry_run first.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "dry_run": { "type": "boolean", "default": true },
      "threshold": { "type": "number", "description": "Override the GC threshold." }
    }
  }
}
```

---

#### `memory_reindex`

```json
{
  "name": "memory_reindex",
  "description": "Rebuild the search index and regenerate embedding vectors. Run after fresh clone, manual file edits, or embedding model change.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "embeddings_only": { "type": "boolean", "default": false }
    }
  }
}
```

### 11.3 Resources

MCP resources allow agents to subscribe to data that changes over time. All resources are **automatically scoped to the current project** via the server's `cwd`. No project identifiers are needed in URIs.

#### `memory://index`

The lightweight memory index (shared + personal merged). Agents can read this at session start to understand what's in the store without loading full memories.

```json
{
  "uri": "memory://index",
  "name": "EngramDB Store Index",
  "description": "Lightweight index of all memories (shared + personal) with summaries, scopes, tags, and scores. Scoped to the current project.",
  "mimeType": "application/json"
}
```

#### `memory://context/{path}`

Dynamic resource that returns memories relevant to a specific file path. Enables proactive context injection.

```json
{
  "uri": "memory://context/{path}",
  "name": "Contextual Memories",
  "description": "Memories relevant to the given file path, scored and sorted. Subscribe to get automatic updates when memories change. Scoped to the current project.",
  "mimeType": "application/json"
}
```

**Example:** An agent subscribing to `memory://context/src/api/auth/handlers.ts` receives all memories (shared and personal) scoped to that file, its parent directories, and matching logical scopes — equivalent to calling `memory_retrieve` with that path.

**Scoping mechanics:** The MCP server resolves project identity on startup from `cwd` (see §5.2). It reads shared memories from `.engramdb/` in the project root and personal memories from `~/.config/engramdb/projects/<project-id>/personal/`. Both are merged transparently — the agent never needs to know where a memory is physically stored.

### 11.4 Prompts

MCP prompts provide reusable prompt templates. The EngramDB exposes prompts that agents can use to guide their memory interactions.

#### `memory-session-start`

```json
{
  "name": "memory-session-start",
  "description": "Orientation prompt for the start of a coding session. Retrieves relevant memories and presents a briefing.",
  "arguments": [
    {
      "name": "path",
      "description": "The file or directory the agent will be working on.",
      "required": false
    }
  ]
}
```

**Generated prompt content:**
```
You are working on a project with a persistent EngramDB.
Before making changes, review these relevant memories:

[auto-injected memory summaries for the given path]

Memories marked with ⚠️ are challenged and may be inaccurate.
Memories marked with 🕐 are flagged for review.

When you discover important patterns, decisions, or hazards during
this session, store them using the memory_create tool.
If you encounter evidence that contradicts an existing memory,
use memory_challenge and ask the user how to resolve it.
```

#### `memory-session-end`

```json
{
  "name": "memory-session-end",
  "description": "End-of-session prompt to review and persist learnings.",
  "arguments": []
}
```

**Generated prompt content:**
```
Before ending this session, consider:
1. Did you make any architectural decisions? → memory_create type: decision
2. Did you discover any hazards or footguns? → memory_create type: hazard
3. Did you encounter non-obvious behavior? → memory_create type: debug
4. Did anything contradict existing memories? → memory_challenge

Current store has N memories (X need review, Y challenged).
Run memory_review if you'd like to address these with the user.
```

### 11.5 Initialization Flow

When the MCP server starts:

```
1. Resolve project identity from cwd (see §5.2)
   ├─ Git remote found → project_id = SHA-256(normalized_remote_url)[:16]
   └─ No git remote    → project_id = SHA-256(absolute_cwd_path)[:16]

2. Update registry at ~/.config/engramdb/registry.json
   └─ Record project name, path, remote, last_accessed

3. Locate .engramdb/ directory in cwd
   ├─ Found → Load manifest.toml, validate schema version
   └─ Not found → Report no EngramDB (tools still available, memory_create will init)

4. Load shared index from .engramdb/index.json
   ├─ Found → Parse, check integrity
   └─ Not found or corrupt → Rebuild from memory files

5. Load personal index from ~/.config/engramdb/projects/<project-id>/personal/index.json
   ├─ Found → Merge with shared index for retrieval
   └─ Not found → No personal memories (normal for first run)

6. Check semantic search availability
   ├─ ONNX model exists at ~/.config/engramdb/models/ → Load model
   │   ├─ LanceDB table exists at projects/<id>/lancedb/ → Ready (full semantic search)
   │   └─ LanceDB table missing → Queue background reindex
   ├─ ONNX model missing + network available → Auto-download model (~23MB), then load
   └─ ONNX model missing + no network → Degraded mode (keyword search + warning)

7. Register tools, resources, and prompts with MCP host

8. Ready
```

**Auto-initialization:** If an agent calls `memory_create` and no `.engramdb/` directory exists, the server automatically initializes the store (equivalent to `engramdb init`) before creating the memory. The global config directory structure is also created as needed.

### 11.6 Error Responses

All tools return structured errors following MCP conventions.

```json
{
  "error": {
    "code": "MEMORY_NOT_FOUND",
    "message": "No memory with id '019...' exists.",
    "data": { "id": "019..." }
  }
}
```

**Error codes:**

| Code                    | Description                                     |
|-------------------------|-------------------------------------------------|
| `MEMORY_NOT_FOUND`      | The specified memory ID does not exist           |
| `VALIDATION_ERROR`      | Input validation failed (details in message)     |
| `STORE_NOT_INITIALIZED` | No .engramdb/ directory and auto-init is off |
| `INDEX_CORRUPT`         | Index is inconsistent — run memory_reindex       |
| `EMBEDDING_UNAVAILABLE` | Embedding model not available, falling back to keyword search |
| `COMPRESS_FAILED`       | Compression requires an LLM and none is configured |
| `CONCURRENT_WRITE`      | File was modified during write (retry recommended) |

### 11.7 Transport

The MCP server supports:

- **stdio** (default): For local agent integrations (Claude Code, Cursor, etc.)
- **SSE (Server-Sent Events)**: For remote or browser-based agents

```bash
# stdio mode (default)
engramdb serve

# SSE mode on custom port
engramdb serve --transport sse --port 3100
```

Configuration in agent settings (e.g., Claude Code `mcp_servers`):

```json
{
  "engramdb": {
    "command": "engramdb",
    "args": ["serve"],
    "cwd": "/path/to/project"
  }
}
```

## 13. CLI UX

### 13.1 Architecture

The CLI operates **directly against files and LanceDB** — no daemon or background process required. The MCP server (`engramdb serve`) is a separate mode that exposes the same logic over stdio/SSE for agent integrations.

```
engramdb <command>     → Reads/writes .engramdb/ and ~/.config/engramdb/ directly
engramdb serve         → Starts MCP server (stdio or SSE), same core logic
```

This is analogous to `git` vs `git daemon` — the tool works standalone, the server is opt-in.

### 13.2 Output Format

Output format is **auto-detected** based on terminal context, with manual overrides:

| Context | Default format | Override |
|---------|---------------|----------|
| stdout is TTY (interactive) | Human-friendly (colored, formatted) | `--format json` or `--json` |
| stdout is piped | JSON (machine-readable) | `--format pretty` |

The `--json` shortcut is available on all commands. `--format` accepts `pretty`, `json`, and `plain` (no color, no decoration).

Global flags available on all commands:
```
--json              Shortcut for --format json
--format <format>   Output format: pretty (default TTY), json (default piped), plain
--no-color          Disable colored output
--quiet, -q         Suppress non-essential output
--verbose, -v       Increase verbosity (debug info, scoring details)
--dir <path>        Override project root (default: cwd, walks up to find .engramdb/)
```

### 13.3 Rich Terminal UI

The CLI uses colored output, Unicode indicators, spinners for long operations, and interactive prompts where appropriate. All rich UI degrades gracefully when piped or when `--no-color` / `--plain` is set.

**Color scheme:**
- 🔴 Red: hazards, errors, deletions
- 🟡 Yellow: warnings, challenged memories, needs_review
- 🟢 Green: success, active memories, high relevance
- 🔵 Blue: info, decisions, conventions
- ⚪ Dim: low-relevance memories, metadata

### 13.4 Commands

---

#### `engramdb init`

Initialize a new EngramDB in the current project.

```bash
engramdb init
```

```
✓ Created .engramdb/
  ├── manifest.toml
  ├── config.toml
  └── memories/
✓ Registered project in ~/.config/engramdb/registry.json
  Project ID: a1b2c3d4e5f6g7h8 (from git remote)
⠋ Downloading embedding model (all-MiniLM-L6-v2, ~23MB)...
✓ Model saved to ~/.config/engramdb/models/all-MiniLM-L6-v2.onnx

EngramDB ready. Add your first memory:
  engramdb add --type convention --content "Describe a coding pattern"
```

Options:
```
--no-embeddings     Skip embedding model download
--template <path>   Initialize with memories from a template bundle
```

---

#### `engramdb add`

Create a new memory. Supports both inline and interactive modes.

**Inline mode:**
```bash
engramdb add \
  --type hazard \
  --content "Never call sync() outside a transaction — causes deadlocks" \
  --physical "src/db/**" \
  --logical "infrastructure.database" \
  --tags "database,transactions" \
  --criticality 0.9
```

```
✓ Created memory 019a3b7c [hazard] (criticality: 0.9)
  "Never call sync() outside a transaction — causes deadlocks"
  scope: src/db/** · infrastructure.database
```

**Interactive mode (no args or `--interactive`):**
```bash
engramdb add
```

```
Type: (use arrows)
  ❯ decision    — Architectural or design choice with rationale
    convention  — Coding standard or pattern
    hazard      — Known pitfall or dangerous pattern
    context     — Background explanation
    intent      — Planned change or temporary state
    relationship — Dependency between components
    debug       — Non-obvious behavior or debugging insight
    preference  — Team or human preference

Content:
  > The retry logic in HttpClient silently swallows 429 responses.
    Check logs manually when debugging rate limit issues.

Summary (auto-generated, press Enter to accept):
  > "HttpClient retry logic silently swallows 429s" ✓

Physical scope (glob, comma-separated, Enter for project-wide):
  > src/lib/http/**

Logical scope (dot notation, comma-separated, Enter to skip):
  > infrastructure.http

Tags (comma-separated, Enter to skip):
  > http, retry, rate-limiting

Criticality (0.0-1.0, default 0.5):
  > 0.7

Visibility: (use arrows)
  ❯ shared    — Committed to repo, visible to team
    personal  — Local only, not committed

✓ Created memory 019a3b7c [debug] (criticality: 0.7)
```

Options:
```
--type, -t <type>         Memory type (required in inline mode)
--content, -c <text>      Memory content (required in inline mode)
--summary, -s <text>      One-line summary (auto-generated if omitted)
--details, -d <text>      Extended details
--details-file <path>     Read details from a file
--physical, -p <glob>     Physical scope(s), comma-separated (default: "/")
--logical, -l <scope>     Logical scope(s), comma-separated
--tags <tags>             Tags, comma-separated
--criticality <0.0-1.0>   Importance (default: 0.5)
--confidence <0.0-1.0>    Certainty (default: 0.8)
--visibility <vis>        "shared" or "personal" (default: "shared")
--interactive, -i         Force interactive mode
--editor, -e              Open $EDITOR for content (useful for long content/details)
```

---

#### `engramdb get <id>`

Show a single memory in full detail.

```bash
engramdb get 019a3b7c
```

```
┌──────────────────────────────────────────────────────────────┐
│  ⚠  HAZARD  019a3b7c                    criticality: 0.95   │
│  "Never call sync() outside a transaction"                   │
├──────────────────────────────────────────────────────────────┤
│  Status: active        Confidence: 1.0                       │
│  Scope:  src/db/**     Logical: infrastructure.database      │
│  Tags:   database, transactions, deadlock, incident          │
│  Source: human         Created: 2026-01-15                   │
│                        Updated: 2026-01-15                   │
│                        Accessed: 2026-02-08                  │
├──────────────────────────────────────────────────────────────┤
│  ## Content                                                  │
│                                                              │
│  The `sync()` method in DatabaseClient acquires a write lock │
│  on the entire connection pool. If called outside a          │
│  transaction, it can deadlock with concurrent reads.         │
│                                                              │
│  This was the root cause of incident INC-2026-003.           │
│  Always wrap in `withTransaction()` first.                   │
├──────────────────────────────────────────────────────────────┤
│  ## Details                                                  │
│                                                              │
│  The incident occurred on 2026-01-14 at 03:42 UTC...         │
│  (23 more lines — use --full or view the file directly)      │
└──────────────────────────────────────────────────────────────┘
```

Options:
```
--full, -f        Show complete details (no truncation)
--raw             Output the raw markdown file contents
--path            Print the file path instead of content
```

ID matching is **prefix-based** — `019a` works if unambiguous, just like git short hashes.

---

#### `engramdb retrieve`

Retrieve memories relevant to a path or context. The primary "what should I know?" command.

```bash
engramdb retrieve --path src/api/auth/handlers.ts
```

```
 6 memories for src/api/auth/handlers.ts                    scope: auth

 0.92 ⚠  HAZARD    019a.. OAuth tokens must be validated server-side only
 0.87 📋 CONVENTION 019b.. All auth endpoints require rate limiting
 0.81 🔗 RELATION   019c.. Auth module depends on Redis for session store
 0.74 📝 DECISION   019d.. Chose JWT with RS256 over HS256 for token signing
 0.68 🐛 DEBUG      019e.. Token refresh has a 2s race window under load
 0.41 💡 CONTEXT    019f.. Auth was ported from Python codebase in 2025

 (use `engramdb get <id>` for full details)
```

```bash
# With semantic query
engramdb retrieve --path src/api/auth/ --query "token expiration"

# Just hazards above 0.8 criticality
engramdb retrieve --path src/db/ --type hazard --min-criticality 0.8

# Summaries only (cheapest)
engramdb retrieve --path src/ --detail-level summary
```

Options:
```
--path <path>              File or directory to scope retrieval
--logical <scope>          Logical scope context
--query, -q <text>         Semantic search query (enables embedding search)
--type, -t <type>          Filter by memory type (repeatable)
--tags <tags>              Filter by tags (comma-separated, OR logic)
--min-criticality <float>  Minimum criticality threshold
--max-results, -n <int>    Max results (default: 10)
--detail-level <level>     "summary", "content" (default), "full"
--include-expired          Include decayed/expired memories
--show-scores              Show score breakdown (semantic, relevance, scope, trust)
```

---

#### `engramdb search <query>`

Full-text and semantic search across all memories, regardless of scope.

```bash
engramdb search "database deadlock"
```

```
 3 results for "database deadlock"

 0.94 ⚠  HAZARD    019a.. Never call sync() outside a transaction
                          src/db/** · infrastructure.database
 0.71 🐛 DEBUG     019g.. Connection pool exhaustion under concurrent writes
                          src/db/pool.rs · infrastructure.database
 0.53 📝 DECISION  019h.. Switched to row-level locking in v2.1
                          src/db/** · infrastructure.database
```

Options:
```
--type, -t <type>          Filter by type
--tags <tags>              Filter by tags
--physical <glob>          Filter by physical scope
--logical <scope>          Filter by logical scope
--min-criticality <float>  Minimum criticality
--max-results, -n <int>    Max results (default: 10)
```

---

#### `engramdb update <id>`

Update an existing memory. Supports inline field updates or opening in `$EDITOR`.

```bash
# Inline field update
engramdb update 019a --criticality 1.0 --tags "database,transactions,critical"

# Open in editor for content changes
engramdb update 019a --editor

# Update content inline
engramdb update 019a --content "Updated: sync() now uses row-level locks as of PR #891"
```

```
✓ Updated memory 019a3b7c [hazard]
  changed: criticality 0.95 → 1.0, tags +critical
```

Options:
```
--content, -c <text>       New content
--summary, -s <text>       New summary
--details, -d <text>       New details
--physical, -p <glob>      New physical scope(s)
--logical, -l <scope>      New logical scope(s)
--tags <tags>              New tags (replaces existing)
--tags-add <tags>          Add tags (appends)
--tags-remove <tags>       Remove tags
--criticality <float>      New criticality
--confidence <float>       New confidence
--visibility <vis>         Change visibility (triggers file move)
--type <type>              Change memory type
--supersedes <ids>         Mark as superseding other memories
--editor, -e               Open the markdown file in $EDITOR
```

---

#### `engramdb delete <id>`

Delete a memory permanently.

```bash
engramdb delete 019a
```

```
  ⚠  HAZARD  019a3b7c  (criticality: 0.95)
  "Never call sync() outside a transaction"

  This will permanently delete this memory. Are you sure? [y/N] y

✓ Deleted memory 019a3b7c
```

Options:
```
--force, -f    Skip confirmation prompt
```

---

#### `engramdb review`

Interactive review of stale and challenged memories. This is the human-in-the-loop maintenance command.

```bash
engramdb review
```

```
 4 memories need attention (2 challenged, 2 stale)

 ─── 1/4 ──────────────────────────────────────────────────────
  ⚠  HAZARD  019a3b7c                     criticality: 0.95
  "Never call sync() outside a transaction"

  Status: CHALLENGED
  Relevance: 0.85 (0.95 - 0.10 challenge penalty)

  ⚡ Challenge from agent (2026-02-10):
  │ "sync() now uses row-level locks as of PR #891,
  │  the deadlock risk no longer applies."
  │ Source: src/db/client.rs

  [k]eep  [u]pdate  [d]elete  [s]kip  [q]uit
  > u

  Opening in $EDITOR...
  ✓ Updated and verified memory 019a3b7c

 ─── 2/4 ──────────────────────────────────────────────────────
  🕐 DEBUG  019e5f6a                       criticality: 0.4
  "Token refresh has a 2s race window under load"

  Status: NEEDS_REVIEW (relevance decayed to 0.28)
  Created: 2025-08-12  Last verified: never

  [k]eep  [u]pdate  [d]elete  [s]kip  [q]uit
  > d

  ✓ Deleted memory 019e5f6a

 ─── Review complete ──────────────────────────────────────────
  Kept: 0  Updated: 1  Deleted: 1  Skipped: 2
```

Options:
```
--scope <scope>     Filter to a logical or physical scope
--type <type>       Filter to a memory type
--challenged-only   Only show challenged memories
--stale-only        Only show needs_review memories
```

---

#### `engramdb challenge <id>`

Manually challenge a memory (typically used by agents via MCP, but available in CLI too).

```bash
engramdb challenge 019a --evidence "sync() now uses row-level locks as of PR #891"
```

```
✓ Challenged memory 019a3b7c [hazard]
  Relevance reduced by 30% until resolved.
  Run `engramdb review` to resolve.
```

Options:
```
--evidence, -e <text>      What contradicts this memory (required)
--source-file <path>       Where the contradiction was found
```

---

#### `engramdb stats`

Overview of the memory store.

```bash
engramdb stats
```

```
 EngramDB — my-app
 ──────────────────────────────────────────
  Memories:    42 (38 shared, 4 personal)
  Status:      36 active, 4 needs_review, 2 challenged

  By type:
    convention   12 ████████████
    decision      9 █████████
    hazard        7 ███████
    context       6 ██████
    debug         4 ████
    relationship  2 ██
    intent        1 █
    preference    1 █

  By scope:
    auth              14
    infrastructure     9
    api-layer          8
    payments           6
    (project-wide)     5

  Embeddings:  ✓ enabled (all-MiniLM-L6-v2, 42 vectors)
  Last reindex: 2026-02-08T14:30:00Z

  Health:
    ⚠  2 challenged memories — run `engramdb review`
    🕐 4 memories below review threshold
    🗑  0 memories below GC threshold
```

---

#### `engramdb gc`

Garbage collect fully decayed memories.

```bash
engramdb gc
```

```
 Dry run — 2 memories below GC threshold (0.05):

  019x.. [debug]  "Temp workaround for build issue" relevance: 0.02
  019y.. [intent] "Migrate to new auth provider"    relevance: 0.01

  Run with --confirm to delete these memories.
```

```bash
engramdb gc --confirm
```

```
✓ Removed 2 memories
```

Options:
```
--confirm           Actually delete (default is dry run)
--threshold <float> Override GC threshold (default: 0.05)
```

---

#### `engramdb compress`

Summarize low-relevance memories into a distilled memory.

```bash
engramdb compress --scope auth
```

```
 Dry run — 5 memories in scope "auth" below threshold (0.4):

  019p.. [debug]      relevance: 0.12  "OAuth state param was missing in v1"
  019q.. [debug]      relevance: 0.18  "Session fixation bug in cookie handler"
  019r.. [intent]     relevance: 0.08  "Plan to migrate from passport.js"
  019s.. [debug]      relevance: 0.22  "CORS preflight issue with auth headers"
  019t.. [context]    relevance: 0.35  "Auth originally used basic auth before OAuth"

 ⠋ Generating summary...

 Proposed summary memory:
 ┌──────────────────────────────────────────────────────────────┐
 │  💡 CONTEXT  (new)                        criticality: 0.35 │
 │  "Auth module history: migrated from basic auth to OAuth,   │
 │   had session fixation and CORS issues (resolved). Previous │
 │   passport.js migration was planned but not completed."     │
 └──────────────────────────────────────────────────────────────┘

  5 source memories will be archived to .engramdb/archive/

  [c]onfirm  [e]dit  [a]bort
```

Options:
```
--scope <scope>      Logical or physical scope to compress
--threshold <float>  Max relevance to include (default: 0.4)
--confirm            Skip dry run (still shows preview before archiving)
```

---

#### `engramdb reindex`

Rebuild the search index and/or regenerate embeddings.

```bash
engramdb reindex
```

```
⠋ Rebuilding index from 42 memory files...
✓ Index rebuilt (42 memories)
⠋ Regenerating embeddings...
✓ 42 embeddings generated in 1.2s
```

Options:
```
--embeddings-only   Skip index rebuild, just regenerate vectors
--index-only        Skip embeddings, just rebuild JSON index
```

---

#### `engramdb serve`

Start the MCP server. Not needed for CLI usage.

```bash
# stdio mode (default, for Claude Code / Cursor / etc.)
engramdb serve

# SSE mode for remote/browser agents
engramdb serve --transport sse --port 3100
```

Options:
```
--transport <mode>   "stdio" (default) or "sse"
--port <port>        Port for SSE mode (default: 3100)
```

---

#### `engramdb list`

List all memories with filtering and sorting.

```bash
# List all memories
engramdb list

# List by type
engramdb list --type hazard

# List by scope
engramdb list --scope auth

# List challenged/stale only
engramdb list --status challenged
```

```
 42 memories in my-app

 ID       Type        Criticality  Status   Summary
 ──────── ─────────── ─────────── ──────── ─────────────────────────────────
 019a..   hazard      0.95         ⚡ chal  Never call sync() outside a tran…
 019b..   convention  0.80         ✓ active All auth endpoints require rate …
 019c..   relationship 0.70        ✓ active Auth module depends on Redis for…
 ...

 (showing 42 of 42 — filter with --type, --scope, --status, --tags)
```

Options:
```
--type, -t <type>       Filter by type
--scope <scope>         Filter by physical or logical scope
--status <status>       Filter: "active", "challenged", "needs_review"
--tags <tags>           Filter by tags
--sort <field>          Sort by: criticality, created, updated, relevance (default: criticality)
--reverse               Reverse sort order
--limit, -n <int>       Max results
```

### 13.5 Shell Completion

EngramDB generates shell completions for bash, zsh, and fish:

```bash
# Generate completions
engramdb completions bash > ~/.bash_completion.d/engramdb
engramdb completions zsh > ~/.zfunc/_engramdb
engramdb completions fish > ~/.config/fish/completions/engramdb.fish
```

Completions include:
- Command and subcommand names
- Flag names and enum values (types, formats, visibility)
- Memory ID prefix completion (reads from index)
- Tag completion (reads from index)
- Logical scope completion (reads from manifest)

### 13.6 Quick Reference

```
engramdb init                       Initialize EngramDB in current project
engramdb add [options]              Create a new memory (inline or interactive)
engramdb get <id>                   Show a single memory in full
engramdb retrieve [options]         Get memories relevant to a path/context
engramdb search <query>             Full-text and semantic search
engramdb list [options]             List all memories with filters
engramdb update <id> [options]      Update a memory (inline or $EDITOR)
engramdb delete <id>                Delete a memory
engramdb challenge <id>             Flag a memory as contradicted
engramdb review                     Interactive review of stale/challenged
engramdb compress [options]         Summarize low-relevance memories
engramdb gc                         Garbage collect decayed memories
engramdb stats                      Store overview and health
engramdb reindex                    Rebuild index and embeddings
engramdb serve                      Start MCP server
engramdb completions <shell>        Generate shell completions
```

## 14. Future Considerations

- **Watch mode**: MCP server watches file changes and proactively surfaces relevant memories.
- **IDE integration**: VS Code extension showing memory annotations inline in the editor.
- **Memory graph**: Visualize relationships between memories (related, supersedes, conflicts).
- **Import/export**: Bulk import from existing CLAUDE.md, ADR files, or inline TODOs. Export as portable bundles.
- **Analytics**: Track which memories are most accessed, which are never retrieved (candidates for pruning), and which correlate with successful vs. failed agent tasks.
