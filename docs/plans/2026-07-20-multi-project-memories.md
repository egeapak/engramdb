# Multi-Project Memories

Status: **accepted design; P0–P2 implemented, P3 deferred**
Date: 2026-07-20
Branch: `claude/multi-project-memories-o4u0vf`

## Problem

Today a memory belongs to exactly **one** project, positionally: its `.md` file
lives in that project's store directory and records carry **no** `project_id`
field. The only tiers are project-shared (in-repo `.engramdb/memories/`),
personal (`<global_data>/projects/<id>/personal/`), and a single machine-wide
**global** store (`__global_store__`). The only cross-store read is
`include_global` (`src/ops/query.rs`): query project ∪ global, dedup by id,
re-sort, truncate.

We want **multi-project memories**: a memory relevant to a *specific set* of
projects (a monorepo family / related repos on one machine) — the tier between
"one project" and "machine-global" — including the ability to share a **single**
memory to an **arbitrary subset** of projects.

## Approaches considered

Three approaches were evaluated by a design panel (storage/consistency,
retrieval/scoring, API/UX-migration) plus an adversarial integrator. All four
independently ranked the same way.

- **A — Duplicate + lineage id.** Copy the full memory into each member store,
  tagged with a shared `group_id`. **Rejected (fatal):** no source of truth;
  UPDATE/DELETE fan out non-transactionally across N `flock`s; copies get
  distinct UUIDs so `merge_scored_memories` (dedups on raw id) double-surfaces
  them; per-store gc/challenge/consolidation diverge permanently with no
  reconciliation.
- **B — Canonical + pointer stubs.** One canonical copy; member stores hold
  content-less pointer stubs, hydrated at read. **Rejected as the primary model
  (fatal cross-machine):** dangling references with no integrity enforcement,
  and content-less stubs are invisible to per-store vector search. *But its
  fatal flaws are cross-machine; on a single machine its precision idea is
  salvageable — see below.*
- **C — Named group store + membership.** Generalize the global store into N
  named group stores that projects subscribe to; fan-in at query time reusing
  `merge_scored_memories`. **Chosen as the substrate:** single source of truth
  per group memory in one store, so every within-store mechanism (`supersedes`,
  challenge/NLI, consolidation, gc/decay, single `flock`, embedding fingerprint)
  keeps working unmodified. It is the literal generalization of the one proven
  fan-in the codebase already ships.

## Decisions (owner-confirmed)

1. **Distribution target: single machine, multi-repo.** Cross-machine git
   distribution (in-repo "hub" store, `origin_group_id` projections) is
   **deferred**, not core. Group stores are machine-local like the global store.
2. **Granularity: per-memory precision is required.** Coarse group opt-in alone
   is insufficient, so C is extended with a per-memory `audience` filter (B's
   precision, without B's content-less stubs — the memory stays a first-class,
   embedded record in one store).
3. **Global = the built-in "everyone" group.** `__global_store__` is unified
   into the group/membership system as the group every project implicitly
   belongs to. One membership concept, one precedence rule.
4. **Precedence when tiers hold the same/contradictory fact: default.** Surface
   all, dedup by id, let scope scoring rank project-local higher — this is
   today's `include_global` behavior. **No new precedence rule.**
5. **Ad-hoc audiences live in the everyone/global store.** When a memory is
   shared to an arbitrary subset `audience=[X,Y]` whose projects share no named
   group, it is written to the everyone/global store and scoped down by
   `audience`. We do **not** mint a store per ad-hoc pair.

## Chosen model: group stores (C) + per-memory `audience` (B's precision)

- **Single source of truth, fully searchable.** A shared memory is a first-class
  record in exactly one store (a named group store, or the everyone/global
  store). Real content ⇒ real embedding ⇒ native semantic recall. All
  within-store lifecycle ops keep working unmodified. No duplication (kills A),
  no content-less pointers (kills B's unsearchable-stub problem).
- **`audience: Option<Vec<String>>` on each memory** — project_ids and/or
  group_ids.
  - `None` ⇒ visible to the whole store's group (coarse default; the common
    case).
  - `Some([...])` ⇒ a group/everyone memory only surfaces for project `P` when
    `audience` contains `P` (or a group `P` is subscribed to). Applied as an
    **index-level filter** at merge time — the same machinery as the existing
    `visibility` column (`audience IS NULL OR audience CONTAINS P`).
- **Membership** lives in `registry.json` as a **new** `subscriptions: Vec<group_id>`
  field on each project entry — **not** overloaded onto `parent_project_id`
  (that is worktree routing; conflating the two misroutes group writes from
  inside a worktree). The everyone/global group is implicit (every project is a
  member) and need not be listed.
- **Read** generalizes `query_memories_with_global` →
  `query_memories_with_extra_stores(project_engine, [everyone, ...subscribed groups])`,
  folding through the existing `merge_scored_memories`. Dedup stays keyed on raw
  `memory.id` and is **correct** (a group memory has exactly one id in one
  store). Fan-in should be parallelized and must distinguish an **empty** store
  from a **corrupt** one (today both are swallowed best-effort).
- **Write** reuses the existing `project` override: MCP `project:"group:X"`,
  CLI `--group X`, routing a single-`flock` atomic write to the group store.
  `SecurityConfig.allow_cross_project_writes` is extended so "session project is
  a member of X" is always-allowed like `global`; non-member is the gated case.
  This authz is **registry-trust-level (advisory)**, documented as such — the
  registry is user-writable and already treated as untrusted input; it is not a
  hard security boundary.
- **Scope hygiene (cross-repo correctness).** Physical scopes are repo-relative
  and meaningless in another repo: **strip physical scope on writes into a
  group/everyone store**, and **suppress the physical-scope multiplier for any
  result whose originating store is a group/everyone store** (flag by
  store-origin, not a new memory field). Logical scopes travel (watch cross-repo
  namespace collision — noted, not yet mitigated).
- **Ranking fairness (deferred to P2).** When a subscribed group's embedding
  fingerprint differs from the project's, raw per-store scores are incomparable.
  Mitigation: a single post-merge cross-encoder rerank pass over the merged
  union (model-agnostic equalizer), skipped when fingerprints match. Recommend
  groups inherit the machine-default embedding model so the daemon stays at one
  resident model (`provider_cache_key` has no store dimension). A `doctor` check
  warns on fingerprint drift.

## Schema change

The only memory-schema change is the nullable **`audience`** field:
`Memory` (frontmatter) + `IndexEntry` (one LanceDB column). Bump
`manifest::CURRENT_SCHEMA_VERSION` `0.3.0 → 0.4.0`; existing stores backfill
`audience = null` by reindexing from the `.md` files on next open (no re-embed) —
the same migration pattern as `has_embedding` / `decay` (R2/R3). A non-member
project with no group memories behaves byte-for-byte as today.

## Phasing

- **P0 (this change) — single machine, precision included.**
  1. `audience` schema field + `0.4.0` migration (`engram-types`, `engram-storage`).
  2. Group-store lifecycle + `group_id` (prefixed SHA of name) + registry
     `subscriptions` (`engram-storage`).
  3. N-way fan-in read with the `audience` index filter; global folded in as the
     everyone group (`src/ops/query.rs`, `src/retrieval/engine.rs`).
  4. Write ergonomics (`--group` / `project:"group:X"`) + membership CLI +
     `allow_cross_project_writes` extension (`engram-cli`, `engram-mcp`).
  5. Scope hygiene: strip physical on group writes; suppress physical multiplier
     for group-origin results.
- **P1 — safety/ergonomics. ✅ implemented.** membership add/remove/list UX
  (`groups subscribe/unsubscribe/members`) with blast-radius confirmation
  (default-decline prompt, `--yes` bypass, JSON-mode gate); parallelized fan-in
  (`join_all`, deterministic merge order); empty-vs-corrupt store distinction
  (`ExtraStoresResult::unreadable`); fingerprint-drift `doctor` check (the
  "Group subscriptions" subsection: readability + embedding-fingerprint
  alignment per subscribed group).
- **P2 — ranking correctness. ✅ implemented.** post-merge rerank equalization
  (`RetrievalEngine::rerank_merged` over the merged union, gated by
  `ops::cross_store_equalization_needed`; same-fingerprint / unstamped-store
  fast path skips the extra cross-encoder pass).
- **P3 (deferred) — git distribution.** in-repo hub store; nullable
  `origin_group_id` frontmatter for one-way regenerable projections; resolvability
  `doctor` check. Still deferred per Decision 1 (cross-machine distribution is
  explicitly out of scope for the single-machine model).

## Non-goals (for P0)

- Cross-machine sync / git distribution of group stores.
- A new precedence/conflict-resolution rule between tiers (default stands).
- Duplicated copies of a memory across stores (rejected — Approach A).
- Content-bearing pointer stubs (rejected — Approach B).
