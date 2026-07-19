# Specification: Epistemic memory classes

Status: **spec approved-pending-review** — supersedes the analysis in
`2026-07-19-epistemic-memory-classes.md` wherever they differ. Implementation
plan and validation/test plan are separate follow-up documents derived from
this spec; nothing here is implemented yet.

## 0. Decision log

Decided by the project owner (2026-07-19):

| # | Decision | Choice |
|---|----------|--------|
| D1 | Spec scope | All phases (1–4) in full detail |
| D2 | Wire/field name | `epistemic` (values `fact` / `observation` / `decision`) |
| D3 | Hazard default class | `Fact` |
| D4 | Situation scoring rollout | Tuned defaults **enabled** out of the box |

Editorial calls made in this spec (flag in review if you disagree):

| # | Call | Rationale |
|---|------|-----------|
| E1 | `origin_task` lives in `Validity`, not `Provenance` | It scopes *validity* ("review when this task ends"); promotion (§11.3) clears it, which would be odd surgery on provenance. |
| E2 | Observation off-diagonal decay: exponential, half-life 90d, floor 0.2 | Conservative; the floor keeps stale observations findable. Tunable in config. |
| E3 | `derived_from` (consolidation justification links) lives in `Validity` | Doctor consumes it for cascade flagging — it is an invalidation condition. |
| E4 | Doctor invalidation checks are report-only by default; `doctor --fix` opts into status flips | Mutating on a read-path command surprises users; matches existing doctor ethos. |
| E5 | Situation profile values pass through a floor transform (trust-style), floor 0.6 | Mirrors `trust_multiplier_floor` exactly; prevents threshold collapse (§7.1). |

## 1. Overview

### 1.1 Problem

`MemoryType` is a topical taxonomy (what a memory is *about*). Memories also
differ on an orthogonal, epistemic axis (what *kind of claim* they make):
structural facts, empirical observations, and rationale-bearing decisions.
This axis determines what invalidates a memory, how it generalizes, how
conflicts should resolve, and when it should be surfaced — and today it is
uncaptured and unused by retrieval.

### 1.2 Goals

1. Capture the epistemic class with near-zero authoring burden.
2. Capture machine-checkable invalidation conditions (premise, watched paths,
   origin task).
3. Use the class in retrieval: situation-conditioned scoring, class-aware
   presentation, class-aware conflict resolution, offline verification.
4. Full backward compatibility: every existing memory file parses unchanged
   and behaves identically unless it opts into new semantics.

### 1.3 Non-goals

- No model inference (NLI/embedding) added to the create or scoring hot paths.
- No repo-state reads at query time — `composite_score` stays a pure function
  of (target, context, config, now).
- No hard deletion on adjudicated contradiction — invalidate/demote only.
- No new epistemic classes beyond the three (a class must change a score,
  curve, or filter to exist).

## 2. Data model (`crates/engram-types`)

New module `crates/engram-types/src/epistemic.rs`, re-exported from `lib.rs`.

### 2.1 `Epistemic`

```rust
/// What KIND of claim this memory makes — orthogonal to [`MemoryType`]
/// (what the memory is ABOUT). Drives decay defaults, situation-conditioned
/// retrieval weighting, conflict-resolution policy, and doctor verification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Epistemic {
    /// Structural fact about the code/tooling as it is. Verifiable against
    /// the repo; flips (rather than fades) when the referenced code changes.
    Fact,
    /// Empirical observation measured at a point in time. Environment-
    /// dependent; goes stale; generalizes with caution.
    Observation,
    /// Normative choice with a rationale. Valid while its premise holds;
    /// binding within its origin scope.
    Decision,
}
```

`Display`/`FromStr` impls follow the `MemoryType` string conventions
(`ops::parse_memory_type` gains a sibling `parse_epistemic`).

### 2.2 `Validity` and `Generality`

```rust
/// First-class invalidation condition: what would falsify this memory.
/// An all-empty Validity is meaningless; writers must emit None instead
/// (enforced by a `Validity::is_empty()` check on the write path).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Validity {
    /// Free-text premise this memory depends on ("while we pin ort rc.12").
    /// Surfaced verbatim to agents; NLI-checkable offline (§10.4, deferred).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premise: Option<String>,

    /// Paths/globs whose modification invalidates this memory. DISTINCT from
    /// `Memory::physical` (where the memory APPLIES, drives scope scoring):
    /// a perf observation may apply to src/retrieval/ but be invalidated by
    /// Cargo.lock changing. Doctor check: §10.1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidated_by: Vec<String>,

    /// Task/feature this memory was created for (free text, human-meaningful,
    /// e.g. "tract-fallback-backend" — NOT a session id; provenance already
    /// carries session_id). Presence means "review when this task completes"
    /// (§11.2); with `generality: task` it also gates injection (§8.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_task: Option<String>,

    /// How far beyond its origin the memory is claimed to hold.
    #[serde(default)]
    pub generality: Generality,

    /// Memory IDs this memory was derived/consolidated from (§11.4). When a
    /// listed memory is invalidated, doctor flags this one for review (§10.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_from: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Generality {
    /// Holds project-wide (default — matches all existing memories).
    #[default]
    Project,
    /// Binding only within its origin task; suppressed/advisory elsewhere.
    Task,
}
```

### 2.3 `Memory` changes (`memory.rs`)

```rust
pub struct Memory {
    // ... existing fields unchanged ...
    /// Epistemic class. Non-optional in the domain model; parsers default it
    /// from `type_.default_epistemic()` for files that predate the field.
    pub epistemic: Epistemic,
    /// Invalidation condition. None = no declared falsifier (like today).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_while: Option<Validity>,
}
```

- `Memory::new` sets `epistemic: type_.default_epistemic()`, `valid_while: None`.
- `MemoryUpdate` gains `epistemic: Option<Epistemic>` and
  `valid_while: Option<Validity>` with matching `apply_to` arms. Setting an
  all-empty `Validity` via update clears `valid_while` to `None`.

### 2.4 Type-derived defaults

```rust
impl MemoryType {
    pub fn default_epistemic(&self) -> Epistemic {
        match self {
            MemoryType::Context | MemoryType::Convention
            | MemoryType::Relationship | MemoryType::Hazard => Epistemic::Fact, // D3
            MemoryType::Debug => Epistemic::Observation,
            MemoryType::Decision | MemoryType::Intent
            | MemoryType::Preference => Epistemic::Decision,
        }
    }
}
```

This mapping is the backward-compatibility anchor. It is **frozen at ship**:
changing it later changes the materialized behavior of old files.

### 2.5 Decay defaults become two-dimensional

Replace create-path call sites of `MemoryType::default_decay()` with:

```rust
pub fn default_decay(type_: MemoryType, epistemic: Epistemic) -> Option<Decay> {
    // INVARIANT (diagonal): default_decay(t, t.default_epistemic())
    //   == t.default_decay()  — byte-identical to pre-epistemic behavior.
    if epistemic == type_.default_epistemic() {
        return type_.default_decay();
    }
    match epistemic {
        // Off-diagonal: the declared class wins over the type default.
        Epistemic::Observation =>
            Some(Decay::exponential(Duration::days(90)).with_floor(0.2)), // E2
        Epistemic::Fact => Some(Decay::none()), // facts flip; they don't fade
        Epistemic::Decision => type_.default_decay(), // premise-bound, not time-bound
    }
}
```

`MemoryType::default_decay()` itself is unchanged and remains public.
Explicit user-provided decay always wins over both defaults (existing
precedence preserved).

## 3. File format (`crates/engram-storage/src/memory_file`)

### 3.1 V2

- `MinimalFrontmatter` (v2.rs:64) gains:
  `#[serde(skip_serializing_if = "Option::is_none")] epistemic: Option<Epistemic>`.
  Rationale for frontmatter (not hidden meta): the class is human-meaningful
  and human-editable, like `type`/`status`.
- `HiddenMeta` (v2.rs:75) gains:
  `#[serde(skip_serializing_if = "Option::is_none")] valid_while: Option<Validity>`.
- **Writer**: emits `epistemic` only when off-diagonal
  (`memory.epistemic != memory.type_.default_epistemic()`); emits
  `valid_while` only when `Some` and non-empty. Diagonal memories therefore
  round-trip byte-identically to files written today.
- **Parser** (`parse_v2`): `epistemic = fm.epistemic.unwrap_or_else(|| fm.type_.default_epistemic())`;
  `valid_while = hidden.valid_while.filter(|v| !v.is_empty())`.

### 3.2 V1

Always defaults: `epistemic` from type, `valid_while: None`. No V1 changes.

### 3.3 Fuzzing

`memory_file` / `memory_file_roundtrip` targets cover the new fields once the
structs carry them. Additional invariant for the roundtrip target: a parsed
memory's `epistemic` is always a valid enum value (serde guarantees) and
diagonal memories write no `epistemic` key.

## 4. Index & manifest (`crates/engram-storage`)

### 4.1 New columns (one migration)

Memories-table additions, in a **single** schema bump
`manifest::CURRENT_SCHEMA_VERSION` `"0.2.0"` → `"0.3.0"`:

| Column | Type | Source | Consumers |
|--------|------|--------|-----------|
| `epistemic` | string (lowercase enum) | `Memory::epistemic` | index filter (§6.1), `ScoreTarget` (§7.1), hook grouping |
| `verified_at` | nullable timestamp | `Memory::verified_at` | fact freshness anchor (§7.3), doctor (§10) |
| `generality` | string | `valid_while.generality` (default `project`) | injection gating (§8.3) without file loads |
| `origin_task` | nullable string | `valid_while.origin_task` | injection gating (§8.3), task ops (§11.2) |

`premise`/`invalidated_by`/`derived_from` do **not** get columns — they are
consumed only by doctor and rendering, both of which read files.

Existing stores backfill via the standard reindex-on-open path (rebuild from
`.md` files, seconds, vectors preserved — same as the R2/R3 `decay` /
`has_embedding` columns).

### 4.2 Projection & score-target parity

`IndexForFiltering` (lance_index.rs:125) gains `epistemic`, `verified_at`,
`generality`, `origin_task`. `ScoreTarget` (scoring/composite.rs:15) gains
`epistemic: Epistemic` and `verified_at: Option<DateTime<Utc>>`.

**Invariant (parity):** `composite_score(&memory, ...)` and
`composite_score_target(target_from_projection, ...)` produce identical
`ScoreBreakdown`s for the same field values. Any future scorer-read field must
land in `Memory`, the projection, and `ScoreTarget` in the same change.

## 5. Create & update surfaces

### 5.1 `ops::CreateParams` (src/ops/create.rs:12)

New fields (all optional; `None` ⇒ defaults):

```rust
pub epistemic: Option<Epistemic>,
pub premise: Option<String>,
pub invalidated_by: Vec<String>,
pub origin_task: Option<String>,
pub generality: Option<Generality>,
```

`create_memory` assembles `valid_while` from the last four (None if all
empty), resolves `epistemic` via `type_.default_epistemic()` fallback, and
uses `default_decay(type_, epistemic)` when no explicit decay was given.

### 5.2 MCP `CreateInput` (crates/engram-mcp/src/server.rs:35)

New optional string/array fields with teaching descriptions (exact copy to be
tuned in review; intent binding):

- `epistemic`: "Epistemic class: fact (structural, verifiable against the
  repo), observation (measured empirically, may go stale), decision (chosen
  over alternatives, valid while its premise holds). Defaults from type; set
  only when it differs."
- `premise`: "Premise this memory depends on, e.g. 'while we pin ort rc.12'.
  State it if the memory becomes wrong when something specific changes."
- `invalidated_by`: "Paths/globs whose change invalidates this memory
  (distinct from physical, which is where it applies)."
- `origin_task`: "Task or feature this was decided for (short human-readable
  name, not a session id)."
- `generality`: "'project' (default) or 'task' (binding only within
  origin_task)."

The create-tool guidance text (server.rs ~2503, ~2536) gains one line:
"3. Did you decide something for THIS task only? -> set origin_task and
generality: task. State the premise ('because C') for decisions."

The `content` description for decisions nudges prose structure: "for
decisions, state what was chosen, over what alternatives, and why."

### 5.3 CLI (`crates/engram-cli/src/app.rs`, `commands/create.rs`)

Flags mirroring MCP: `--epistemic <fact|observation|decision>`,
`--premise <text>`, `--invalidated-by <glob>` (repeatable),
`--origin-task <name>`, `--generality <project|task>`. `update` gains the
same flags plus `--clear-validity`.

### 5.4 Output surfaces

`--format json` / MCP responses include `epistemic` always and `valid_while`
when present. `pretty` output shows `[fact]`-style class tag next to the
existing type tag only when off-diagonal.

## 6. Query surfaces & filtering

### 6.1 Hard filter

`RetrievalQuery` (src/retrieval/engine.rs:73) gains
`epistemic: Option<Vec<Epistemic>>`, applied index-level exactly like the
existing `types` filter (filters.rs + `build_filter_predicate`). Exposed as
MCP `QueryInput.epistemic` (array of strings) and CLI `--epistemic`
(repeatable). Applies in both Rank and Filter modes.

### 6.2 Situation

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Situation { SessionStart, FileEdit, Debugging, DesignChoice }
```

- `RetrievalQuery.situation: Option<Situation>`; threaded into
  `ScoringContext.situation`.
- Sources: SessionStart hook sets `SessionStart`; pre-tool-use hook sets
  `FileEdit`; MCP `QueryInput.situation` / CLI `--situation` accept all four
  (agents self-declare `debugging` / `design_choice` — hooks cannot detect
  them).
- `None` ⇒ multiplier 1.0 (neutral); all existing queries unchanged.

## 7. Scoring (`src/scoring`)

### 7.1 Situation multiplier

Applied in `composite_score_inner` immediately after the trust multiplier,
before the challenge penalty:

```
score = base * scope_mult * trust_mult * situation_mult - challenge_penalty
situation_mult = 1.0                                  if situation is None
               = floor + (1 - floor) * profile[situation][target.epistemic]
                                                       otherwise
```

`floor = retrieval.scoring.situation.floor`, default **0.6** (E5). With the
default floor, profile value `p` yields effective multiplier `0.6 + 0.4p` ∈
[0.6, 1.0] — a class can be down-weighted at most 40%, and a criticality-0.8
exact-scope memory at the lowest profile value still scores
`0.8 × 0.8 = 0.64 > relevance_threshold (0.45)`.

Tuned default profiles (D4 — shipped enabled):

| profile value | fact | observation | decision |
|---|---|---|---|
| `session_start` | 1.0 | 0.5 | 0.8 |
| `file_edit` | 0.7 | 0.7 | 1.0 |
| `debugging` | 0.6 | 1.0 | 0.7 |
| `design_choice` | 0.8 | 0.7 | 1.0 |

Interactions:
- Applies identically in all four weight modes (with_keyword / with_query /
  degraded / scope_only) — it is a post-multiplier, mode-independent.
- Applies pre-threshold and pre-rerank (engine Steps 6–8), so rerank-blend
  semantics are untouched and the reranker sees class-appropriate survivors.
- Situation profiles are class-conditioned **constants** — never
  age-conditioned. Decay remains the only age-sensitive channel
  (no-double-penalty rule).
- `ScoreBreakdown` gains `situation_multiplier: f64` (1.0 when neutral) for
  transparency; rendered by explain/debug output like the other multipliers.

Validation: each profile value and the floor must be in [0,1]
(`SituationConfig::validate`, wired wherever `ScoringWeights::validate` is
called); non-finite config values clamp to neutral 1.0 on read.

### 7.2 Per-class challenge penalty

`ScoringConfig.challenge_penalty` (config.rs:118) changes type from `f64` to:

```rust
#[serde(untagged)]
pub enum ChallengePenalty {
    Flat(f64),                         // legacy: challenge_penalty = 0.10
    PerClass { fact: f64, observation: f64, decision: f64 },
}
```

Defaults (PerClass): **fact 0.15, observation 0.20, decision 0.05**.
Semantics unchanged otherwise: flat subtraction when
`status == Challenged`, then the existing [0,1] clamp. `Flat(x)` applies `x`
to all classes — existing `config.toml` files parse and behave as before.
Values clamp to [0,1] on read (fuzzer ignores validation).

Rationale for the ordering: a contradicted observation is cheap to re-measure
(suppress hardest); a contradicted fact is probably stale (moderate); a
contested decision must remain visible *together with* its dispute (mildest).

### 7.3 Fact freshness anchor

In `composite_score_inner`, the decay anchor becomes:

```rust
let anchor = match target.epistemic {
    Epistemic::Fact => target.verified_at.unwrap_or(target.created_at),
    _ => target.created_at,
};
let decay_factor_value = decay::decay_factor(anchor, now, target.decay);
```

Effect today is nil for diagonal facts (their default decay is `None`), but it
makes verification meaningful for any fact with a decay curve, and doctor's
`verified_at` writes (§10) become score-relevant without new formula terms.
Observations and decisions keep `created_at` — re-verifying an observation is
modeled instead by updating it (which creates a fresh memory state), and
decisions are premise-bound.

## 8. Presentation (hook context injection, `crates/engram-cli/src/commands/hook.rs`)

### 8.1 Grouping & ordering

`group_by_type` generalizes to group by epistemic class (primary header) with
the type as a per-line tag. Class order per situation:

- SessionStart: facts → decisions (project-wide) → observations.
- FileEdit (pre-tool-use): decisions/hazard-facts first (existing
  hazard-first behavior preserved via type tag ordering inside the fact
  group), then facts, then observations.

### 8.2 Per-class rendering (`format_memory_entry`)

- **Decision**: `- {summary} — because {premise}` and, when set,
  `; revisit if {invalidated_by joined}`. When `premise` is absent, render
  summary only (never invent).
- **Observation**: `- {summary} (observed {created_at:%Y-%m-%d})`, plus
  `, verified {verified_at:%Y-%m-%d}` when set.
- **Fact**: compact one-liner; append `(verified {date})` only when
  `verified_at` is set.

`premise`/`invalidated_by` live in the file, not the index; the hook loads
files only for the ≤ N survivors after truncation (bounded, matches the
engine's materialize-survivors-only pattern).

### 8.3 Task-generality gating

Memories with `generality == Task` are **suppressed** from hook injection
unless the current session's task matches `origin_task` (session→task mapping
per §11.1; absent a mapping, task-scoped memories are suppressed from hooks
but remain retrievable by explicit query). Implemented from the index columns
(§4.1) — no file loads for suppressed entries.

### 8.4 Budget policy (`format_detailed_context_with_budget`, budget 2000)

- Facts are compressible: summary line only, preview dropped first.
- Decisions are **atomic**: if `summary + because-clause` doesn't fit the
  remaining budget, skip the entire entry (never truncate the rationale
  mid-clause).
- Observations: summary + date only.

Config: `[hooks] class_order` override (optional, default per §8.1).

## 9. Conflict semantics (`crates/engram-models/src/nli/challenge.rs` + engine ingestion tail)

### 9.1 Routing table

`challenge_for_contradictions` gains the new memory's `(Epistemic, id)` and
each target's `Epistemic` (callers in the engine ingestion tail hold both
memories). Resolution by (new, existing) class pair:

| New \ Existing | Fact | Observation | Decision |
|---|---|---|---|
| **Fact** | standard challenge (stale-fact) | challenge observation | premise challenge on decision |
| **Observation** | stale-fact challenge on the fact | challenge older side (§9.2) | premise challenge on decision |
| **Decision** | challenge fact | challenge observation | **supersession candidate**: existing → `NeedsReview` |

- *Supersession candidate*: existing decision gets `Status::NeedsReview`
  (deliberately zero score penalty — "probably replaced, human confirms") and
  a challenge record naming the new memory id; doctor lists it as
  "resolve: supersede (`update --supersedes`) or reject". Never auto-writes
  `supersedes`.
- *Premise challenge*: mild — challenge text
  `"premise may have changed: contradicted by <new-id> (NLI {score:.2})"`;
  decision-class penalty (0.05) applies; §8.2 rendering shows the rationale
  next to the dispute.
- *Stale-fact challenge*: evidence includes the contradicting memory's date.

### 9.2 Entrenchment order

When the table alone doesn't pick a side (observation-vs-observation,
fact-vs-fact), the side that **yields** (gets challenged) is chosen by, in
order: lower provenance trust class (human > agent > imported > inferred),
then older `verified_at.unwrap_or(created_at)`, then lower `confidence`.
Ties: challenge the existing memory (status quo: new information wins).

### 9.3 Unchanged guarantees

Same atomic `update_with` write path, same best-effort/logged error handling,
same background execution. No NLI on the scoring path.

## 10. Doctor & verification (`src/ops/doctor.rs`)

All checks are report-only by default; `doctor --fix` applies the indicated
status flips (E4). Both checks skip memories without the relevant fields, so
pre-existing stores produce zero new findings.

### 10.1 Invalidated-path check

For every memory with non-empty `valid_while.invalidated_by`: compute the
newest modification time of files matching the globs (file mtime in v1 of the
check; git commit times as a later refinement — worktree routing via
`resolve_project_root` applies) and compare against
`verified_at.unwrap_or(created_at)`. Newer ⇒ finding
`"invalidation paths changed since last verification"`; `--fix` flips to
`NeedsReview`.

### 10.2 Stale-observation check

Observations with `verified_at.unwrap_or(created_at)` older than
`[epistemic] observation_review_days` (default 90) ⇒ finding
`"re-verify or delete"`. No status flip even under `--fix` (age alone is not
evidence of wrongness; decay already handles ranking).

### 10.3 Derived-from cascade

For memories with `valid_while.derived_from`: if any listed id is missing,
`Challenged`, or `NeedsReview` ⇒ finding
`"a source this memory was derived from is invalid"`; `--fix` flips to
`NeedsReview`. One level only — no transitive propagation (TMS-lite).

### 10.4 Verification write path

`engramdb verify <id>` (new op, CLI + MCP): stamps `verified_at = now` and,
if status was `NeedsReview` due to §10.1/§10.3, resets to `Active`. This is
the human/agent counterpart to the checks; facts' freshness anchor (§7.3)
consumes it. (Offline NLI premise-vs-repo checking is explicitly deferred —
not in this spec's scope.)

## 11. Lifecycle (Phase 4 — full detail per D1)

### 11.1 Task identity

Tasks are free-text names (`origin_task`). A session↔task association is
established when the agent passes `origin_task` on create, or via a new MCP
tool/CLI `task current <name>` that records the mapping in the project's
`.engramdb/` state (small JSON, keyed by provenance `session_id`). No global
task registry; unknown tasks are just strings.

### 11.2 Completion → demotion

New op `engramdb task complete <name>` (CLI + MCP tool): for every memory
with `valid_while.origin_task == name`:

- `generality == Task`: flip decay to `Exponential(14d)` (the Intent curve)
  unless the memory has an explicit user-set decay. Demotion **replaces** the
  curve — it cannot stack with other penalties.
- `generality == Project`: no decay change; emit a doctor-style notice
  ("project-wide memory from completed task <name> — verify or demote").

Runs under the per-project write lock; each demotion is an ordinary
`update_with`. Optionally invoked from a SessionEnd hook when a session↔task
mapping exists (config `[epistemic] demote_on_session_end`, default false).

### 11.3 Re-confirmation → promotion

The telemetry collector already records per-session retrieval outcomes. A
maintenance-pass job (§11.5) counts, per task-bound memory, the distinct
sessions (excluding the origin session) in which it was retrieved at or above
the relevance threshold. At `promotion_min_sessions` (default 3):

- v1 behavior: doctor **suggestion** only ("retrieved in N later sessions —
  promote to project-wide?").
- Behind `[epistemic] auto_promote = true` (default false): clear
  `origin_task`, set `generality: Project`, reset decay to the diagonal
  default, stamp `verified_at`.

Promotion never retypes the memory (Decision stays Decision; retyping to
Convention is a human edit).

### 11.4 Consolidation (observations → fact)

Extends `ops::compress` rather than a new subsystem: a consolidation pass
finds clusters of ≥ `consolidation_min_sources` (default 3) `Active`
observation-class memories with pairwise embedding similarity ≥
`consolidation_similarity` (default 0.85) and no pairwise NLI contradiction.
For each cluster it (suggestion-first, same auto flag as §11.3):

- creates a Fact-class memory (type `context` unless all sources share a
  type) with `valid_while.derived_from = [source ids]`,
  `provenance: inferred`, criticality = max(sources), decay = none;
- **demotes** the sources: decay flipped to `Exponential(30d)` floor 0.1 —
  never deleted (sources are the evidence; §10.3 depends on them).

Model-dependent steps run only where providers already run (daemon or
in-process maintenance with graceful fallback: no providers ⇒ pass skips).

### 11.5 Scheduling

All lifecycle jobs run in the existing throttled maintenance pass
(`MaintenanceConfig`, default 6h interval) — main-worktree invocations only,
same override ladder. When the shared daemon is resident, the maintenance
pass may run there on idle (Letta-style), but the daemon is **not** required:
graceful fallback is the contract.

## 12. Config surface (complete reference)

```toml
[retrieval.scoring.situation]
floor = 0.6                      # [0,1]; effective mult = floor + (1-floor)*p

[retrieval.scoring.situation.session_start]
fact = 1.0
observation = 0.5
decision = 0.8

[retrieval.scoring.situation.file_edit]
fact = 0.7
observation = 0.7
decision = 1.0

[retrieval.scoring.situation.debugging]
fact = 0.6
observation = 1.0
decision = 0.7

[retrieval.scoring.situation.design_choice]
fact = 0.8
observation = 0.7
decision = 1.0

# Replaces the scalar; scalar `challenge_penalty = 0.10` still parses (Flat).
[retrieval.scoring.challenge_penalty]
fact = 0.15
observation = 0.20
decision = 0.05

[epistemic]
observation_review_days = 90     # §10.2
observation_half_life_days = 90  # E2 off-diagonal default
observation_decay_floor = 0.2
demote_on_session_end = false    # §11.2
promotion_min_sessions = 3       # §11.3
auto_promote = false
consolidation_min_sources = 3    # §11.4
consolidation_similarity = 0.85
auto_consolidate = false

[hooks]
# class_order = ["fact", "decision", "observation"]   # optional override, §8.1
```

All sections/fields `#[serde(default)]` — absent config ⇒ the defaults above.
None of these fields affect which models load ⇒ `provider_cache_key`
unchanged. Doc-comment on `NliConfig`: if class-specific NLI models are ever
added, the cache key must be extended.

## 13. Compatibility & migration summary

| Surface | Mechanism | Behavior change for existing data |
|---|---|---|
| `.md` files | serde defaults + type-derived materialization | none; fields stamp on next natural rewrite |
| LanceDB | one bump 0.2.0→0.3.0, reindex-on-open backfill | none (columns derived from files) |
| Scoring | diagonal decay invariant; neutral-on-missing situation | none without `situation`; with hooks: tuned profiles active (D4) — the **only** intended behavior change on upgrade |
| Config | untagged `ChallengePenalty`; defaulted sections | none for existing configs except new per-class penalty defaults when no scalar is set |
| MCP/CLI | all new params optional | none |

Note on D4: upgrading changes hook-driven ranking (session-start and
pre-tool-use queries now carry a situation). This is intentional and called
out in release notes; `floor = 1.0` is the one-line opt-out.

## 14. Invariants & acceptance criteria (test-plan feed)

1. **Diagonal decay invariant**: `default_decay(t, t.default_epistemic()) ==
   t.default_decay()` for all 8 types (paired test, mirroring the NLI-default
   drift tests).
2. **Roundtrip**: for every class × validity combination, write→parse is
   identity; diagonal memories emit no `epistemic` key; pre-epistemic fixture
   files parse with the §2.4 defaults.
3. **Score parity**: `composite_score` ≡ `composite_score_target` on
   identical field values, including `epistemic`/`verified_at` (extend the
   existing parity coverage).
4. **Neutrality**: `situation: None` ⇒ breakdown byte-identical to
   pre-change scoring for every mode; `Flat` penalty config ⇒ identical to
   current behavior.
5. **Threshold composition (worst case)**: exact-scope, criticality 0.8,
   human, unchallenged memory stays ≥ `relevance_threshold` under every
   default profile value (mirror `test_trust_floor_prevents_extreme_compounding`).
6. **No double penalty**: demotion replaces the decay curve; situation
   profiles contain no age terms (asserted structurally: profile is
   `f64` per class, nothing else).
7. **Conflict routing**: decision-vs-decision never sets `Challenged`
   (only `NeedsReview`); no routing path deletes a memory.
8. **Doctor is read-only without `--fix`**: store contents hash-identical
   after plain `doctor` runs including the new checks.
9. **Hook budget**: decision entries are never truncated mid-rationale;
   task-generality memories absent from hook output for non-matching
   sessions; output ≤ 2000 chars.
10. **Fuzz**: `composite_score` target extended with epistemic discriminant,
    situation selector, and per-class penalties as raw `f64`s — finiteness
    assertion holds; `memory_file` targets cover `Validity`.
11. **Graceful degradation**: consolidation/verification passes are no-ops
    (with logged notice) when providers are unavailable; daemon absence never
    breaks any lifecycle op.
12. **CI gates**: `cargo fmt --all`, `cargo clippy --workspace --all-targets
    --all-features -- -D warnings`, `cargo nextest run --workspace
    --all-features` all pass at every increment.

## 15. Deferred / explicitly out of scope

- NLI premise-vs-repo verification (offline classifier for §10.4).
- Bi-temporal validity intervals (`valid_from`/`invalidated_at` columns) —
  status + challenges + supersedes cover current needs; revisit if history
  queries become a requirement.
- Epistemic auto-classification suggestions in doctor.
- Transitive derived-from propagation.
- Retyping on promotion.
