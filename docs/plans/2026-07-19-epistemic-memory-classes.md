# Epistemic memory classes: capturing *what kind of claim* a memory makes

Status: proposal (analysis + design ideas; nothing implemented)

## 1. Problem

Memories in EngramDB differ in their *epistemic character*, and the schema does
not capture it:

1. **Structural facts** — "this project uses X and Y tooling." True until the
   code changes; verifiable against the repo; low controversy.
2. **Empirical observations** — "we observed X is more performant than Y."
   Evidence-backed, measured at a point in time, invalidated by environment
   changes (dependency bumps, hardware, dataset); generalizes with caution.
3. **Task-scoped decisions** — "we decided A over B because C." Normative,
   carries a rationale and rejected alternatives, valid while premise C holds;
   binding within its origin task but not automatically beyond it.

These differ in **what invalidates them**, **how they generalize**, **what
"confidence" means**, **how conflicts should resolve**, and **when an agent
should be shown them**. `MemoryType` (decision / convention / hazard / context /
intent / relationship / debug / preference) is a *topical* taxonomy — what the
memory is about — and conflates this second, epistemic axis. A `hazard` can be
a verified fact or an unconfirmed observation; a `convention` is often a frozen
decision whose rationale has been lost.

### What the code does today

- `MemoryType`'s only mechanical effects: `default_decay()`
  (`crates/engram-types/src/memory.rs:45`), the hard `types` query filter, and
  the grouping header in hook context injection (`hook.rs::group_by_type`).
- The composite score (`src/scoring/composite.rs`) —
  `base * scope_mult * trust_mult - challenge_penalty` — is entirely
  type-blind. The challenge penalty is one flat scalar regardless of what kind
  of claim is being contested.
- `Provenance.reason`, `supersedes`, and `verified_at` exist but are
  informational only; nothing in scoring or doctor reads `verified_at`.
- The NLI challenge flow (`crates/engram-models/src/nli/challenge.rs`)
  challenges contradictions symmetrically — it cannot express "the observation
  is stale" vs "the decision is being re-litigated."
- `compress` consolidates by criticality threshold only; there is no
  episodic→semantic promotion.

## 2. Prior art (condensed)

- **Cognitive science (Tulving; complementary learning systems):** semantic vs
  episodic vs procedural memory maps directly onto fact / observation /
  decision-rationale. Consolidation — repeated consistent episodes promoted
  into one semantic fact, with the episodes retained as provenance — is the
  standard mechanism.
- **Zep/Graphiti (arXiv:2501.13956):** bi-temporal facts (`valid_at` /
  `invalid_at` separate from `created_at`); contradiction at ingest *closes the
  old fact's validity window* rather than deleting it. History stays queryable;
  default retrieval filters to currently-valid.
- **LangMem:** the discipline that matters — each memory class must have a
  *distinct storage/retrieval behavior*, not just a tag. A class that doesn't
  change a score, a decay curve, or a filter shouldn't exist.
- **Mem0:** write-time reconciliation (ADD/UPDATE/DELETE/NOOP against nearest
  neighbors). Cautionary: hard DELETE on LLM-adjudicated contradiction is
  unrecoverable; prefer Graphiti-style invalidation.
- **ADRs (Nygard):** decisions need a different schema from facts — the
  rationale and rejected alternatives are the payload (the decision itself is
  visible in the code; the *why* is not) — plus a status lifecycle
  (accepted → deprecated → superseded, with forward links, never edits).
- **Design-rationale literature (Grudin):** capture burden kills adoption.
  The writer pays the structuring cost, future readers get the benefit. Any
  scheme requiring rich forms at write time yields empty/garbage fields. One
  enum with an inferable default is the ceiling.
- **Truth-maintenance systems (JTMS):** beliefs carry justification links;
  premise retraction *flags* dependents. The applicable subset is a nullable
  `derived_from`/premise field plus a doctor pass — never a full inference
  engine.
- **AGM belief revision:** contradiction resolution needs a precedence order
  (entrenchment): source class, then validity recency, then confidence —
  instead of a symmetric penalty.

## 3. Recommended design

### 3.1 Schema: `Epistemic` discriminant + `Validity` condition

Two additions to `Memory` (new module `crates/engram-types/src/epistemic.rs`).
Only machine-actionable data earns frontmatter; rationale/evidence/alternatives
remain prose in `content`/`details` (nudged by tool-schema descriptions).

```rust
/// What KIND of claim this memory makes — orthogonal to MemoryType (what the
/// memory is ABOUT). Drives decay defaults, conflict policy, and retrieval.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Epistemic {
    /// Structural fact, verifiable against the repo; flips when code changes.
    Fact,
    /// Empirical observation measured at a point in time; goes stale.
    Observation,
    /// Normative choice with a rationale; valid while its premise holds.
    Decision,
}

/// First-class invalidation condition: what would falsify this memory.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Validity {
    /// Free-text premise ("while we pin ort rc.12"). Surfaced verbatim to
    /// agents; NLI-checkable offline later.
    pub premise: Option<String>,
    /// Paths/globs whose modification invalidates this memory. Distinct from
    /// `physical` (where it APPLIES): a perf observation may apply to
    /// src/retrieval/ but be invalidated by Cargo.lock changing.
    pub invalidated_by: Vec<String>,
    /// Task/feature this was made for; reviewed on task completion.
    pub origin_task: Option<String>,
    /// How far beyond its origin it holds: project (default) | task.
    pub generality: Generality,
}
```

`Memory` gains `epistemic: Epistemic` (non-optional in the domain model) and
`valid_while: Option<Validity>`.

### 3.2 Defaults: zero authoring burden on the diagonal

`MemoryType::default_epistemic()` — Context/Convention/Relationship/Hazard →
Fact; Debug → Observation; Decision/Intent/Preference → Decision. Parsers
materialize missing fields from this mapping, so **every existing file parses
and behaves byte-identically**. The field only needs to be spelled out for
off-diagonal cases (a convention that is really a task-scoped decision; a
hazard that is an unverified observation).

Decay defaults become `default_decay(type_, epistemic)` with the invariant
`default_decay(t, t.default_epistemic()) == t.default_decay()` (assert in a
paired test). Off-diagonal: Observation → exponential (~45–90d, floor ~0.2);
Fact → none (facts flip, they don't fade); Decision → type default
(premise-bound, not time-bound).

No NLI auto-classification on the create path (create is deliberately
non-blocking; the NLI model is an entailment model, not a classifier; silent
misclassification silently changes behavior). Offline `doctor` *suggestions*
("this convention reads like an observation") are acceptable later.

### 3.3 Retrieval integration (phased)

**Phase 1 — presentation + filters (no scoring change).**
- `epistemic` filter param mirroring `types` in `RetrievalQuery`, MCP
  `QueryInput`, CLI.
- Hook injection (`hook.rs`): group by class; render decisions atomically as
  `summary — because {premise/rationale}; revisit if {invalidate_when}` (never
  truncate the "because" mid-clause — a decision without its why is the
  failure mode this design targets); observations get
  `(observed {date})`; facts stay compact one-liners, `verified {date}` when
  set. Suppress `generality: task` memories whose `origin_task` doesn't match.
  Budget rule: facts compress, decisions are atomic (skip whole entry if it
  doesn't fit), observations get summary+date.

**Phase 2 — situation-conditioned scoring (opt-in, neutral by default).**
`RetrievalQuery`/`ScoringContext` gain
`situation: Option<Situation> { SessionStart, FileEdit, Debugging, DesignChoice }`;
hooks set the first two; agents declare the rest via a new MCP/CLI param. A
fourth post-multiplier mirroring the trust transform:

```
score = base * scope_mult * trust_mult * situation_mult - challenge_penalty
situation_mult = floor + (1 - floor) * profile[situation][epistemic]   // floor ≈ 0.6
```

Config `[retrieval.scoring.situation.*]` tables (e.g. debugging boosts
observations, file_edit boosts decisions/hazards, session_start boosts facts).
All-1.0 defaults → byte-identical behavior until configured. The floor is
load-bearing: the `scope_multiplier_floor` history
(`test_logical_only_exact_match_exceeds_default_threshold`) shows unfloored
multipliers collapse everything below `relevance_threshold` (0.45).

**Phase 3 — class-aware conflict + verification.**
- **Directional challenge routing** in `challenge_for_contradictions`:
  decision-vs-decision → supersession candidate (`NeedsReview`, which carries
  no score penalty — correct for "probably replaced, confirm"); observation-vs-
  fact → stale-fact challenge; observation-vs-decision → premise challenge
  (mild; render the rationale next to the dispute). Entrenchment order for
  which side yields: source class (human > tool-observed > inferred), then
  validity recency, then confidence.
- **Per-class challenge penalty** replacing the scalar
  (`fact 0.15 / observation 0.20 / decision 0.05` — contested decisions must
  stay visible *with* their dispute). Untagged serde enum keeps
  `challenge_penalty = 0.10` configs parsing.
- **Doctor checks**: (a) memories with `invalidated_by` whose matching files
  changed after `verified_at.unwrap_or(created_at)` → report / opt-in flip to
  `NeedsReview` (mtime first, git times later); (b) observations unverified for
  > `observation_review_days` (90) → "re-verify or delete". Verification
  writes `verified_at`; for facts, scoring can then anchor decay/freshness on
  `verified_at` instead of `created_at`. Premise checking is **offline only**
  — never NLI or repo greps in the scoring hot path (breaks the hooks'
  no-provider guarantee and projection-only scoring).

**Phase 4 — lifecycle (most speculative).**
- **Task demotion:** on task completion, task-bound decisions flip decay
  `None → Exponential(14d)` (reusing the Intent machinery — demotion replaces
  the curve, so it can't stack with other penalties).
- **Promotion:** telemetry already records per-session retrieval outcomes; a
  maintenance pass that sees a task-bound decision retrieved above threshold in
  N distinct later sessions suggests (doctor first, auto behind a flag)
  clearing `origin_task`, resetting decay, retyping to Convention, stamping
  `verified_at`.
- **Consolidation:** N mutually-consistent observations supporting one
  regularity → synthesize a fact with `derived_from: [ids]`, demote (never
  delete) the sources. The shared daemon's idle watchdog is the natural host
  (Letta "sleep-time compute" pattern). Keeping `derived_from` is what lets
  the fact be revised later — deleting evidence is the ChatGPT-memory failure
  mode.

### 3.4 Compatibility & migration

- **Files:** `#[serde(default)]` optional fields in V2 `HiddenMeta`; V1 and
  field-less V2 files default from `type_`. No rewrite ever required; fields
  stamp on next natural update. `memory_file` fuzz targets cover the new
  fields for free.
- **Index:** one migration — add `epistemic` (+ `verified_at` when Phase 2/3
  scoring needs it) to the memories table and `IndexForFiltering`/`ScoreTarget`
  together, bump `manifest::CURRENT_SCHEMA_VERSION` 0.2.0 → 0.3.0 (same
  reindex-on-open path as the `decay`/`has_embedding` columns). **Parity
  test:** `composite_score` vs `composite_score_target` must stay
  byte-identical. `Validity` needs no column (doctor reads files; not a filter
  axis).
- **ProviderCache:** untouched (no model-affecting config). Flag in the config
  doc-comment: if class-specific NLI models ever appear, extend
  `provider_cache_key`.
- **Fuzzing:** extend `composite_score` target with the class discriminant,
  situation, and per-class penalties as raw f64s; clamp config-derived values
  on read.

### 3.5 Risks

- **Threshold collapse:** every sub-1.0 multiplier shifts mass under the fixed
  0.45 threshold. Floors + a worst-case composition test (mirror
  `test_trust_floor_prevents_extreme_compounding`).
- **Double-penalizing staleness:** decay is the *only* age-sensitive channel;
  situation profiles are class-conditioned constants; demotion replaces
  curves.
- **Environment-conditioned decay is a trap:** scoring must stay a pure
  function of (memory, context, config, now) — repo-state-dependent scoring is
  non-deterministic and breaks projection scoring. The honest version is
  steeper wall-clock decay + `invalidated_by` doctor checks.
- **Over-taxonomization:** three classes, each with distinct behavior, is the
  cap. A class that doesn't change a score/curve/filter doesn't exist.
- **Hard deletion on adjudicated contradiction:** never — invalidate/demote
  with audit trail; the file-on-disk model makes keep-but-demote nearly free.

## 4. Open questions

1. **Hazard's default class** — Fact (verifiable, floor-0.5 no-decay already
   says "never forget") vs Observation (field-discovered footguns). One line to
   change, but frozen into materialized defaults once shipped.
2. **Where `origin_task` lives** — `Validity` (validity scope) vs `Provenance`
   (creation context, next to `session_id`). If memories outlive their task
   after re-verification, provenance is the better home.
3. **Wire naming** — `epistemic` is precise but jargon-y (`basis`/`nature`
   friendlier); and does `epistemic: decision` next to `type: decision` help
   (self-explanatory default) or confuse the authoring agent? Worth a
   prompt-level A/B before serde names freeze into the file format.
4. **Should class ever enter `base`** (beyond the situation multiplier)?
   Deliberately no for now — decay refinement + status flips + filtering keep
   `composite_score` auditable.

## 5. Suggested first increment (one PR)

1. `Epistemic` + `Validity` types, `default_epistemic()` /
   two-arg `default_decay()` with the diagonal-invariant test; parser
   defaulting in `memory_file` (zero behavior change).
2. Create surfaces: MCP `CreateInput` fields with teaching descriptions
   (`epistemic`, `premise`, `invalidated_by`, `origin_task`, `generality`),
   CLI flags, `CreateParams`.
3. Schema bump 0.3.0 (`epistemic` + `verified_at` columns) + `epistemic` query
   filter + `ScoreTarget` parity test.
4. Phase-1 hook rendering (decision atomicity, observation dating,
   task-generality suppression).

Everything scoring-related (situation multiplier, penalty table, challenge
routing, doctor checks, lifecycle) layers on top without further schema
changes.
