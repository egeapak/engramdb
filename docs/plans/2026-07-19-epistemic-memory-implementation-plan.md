# Implementation plan: Epistemic memory classes

Derived from `2026-07-19-epistemic-memory-spec.md` (the spec is
authoritative; on any conflict, the spec wins and this plan gets fixed).
Companion: `2026-07-19-epistemic-memory-test-plan.md`.

## 0. Ground rules

- **Increment gates** (every increment, per CLAUDE.md): `cargo fmt --all`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo nextest run --workspace --all-features`. Web-sandbox builds need the
  documented protoc/ORT/model-staging workarounds.
- **DAG discipline**: edges point inward only. New code obeys the existing
  layering: `engram-types` ← `engram-storage` ← `engram-models` ← core
  (`ops`/`retrieval`/`scoring`/`daemon`) ← `engram-mcp`/`engram-cli`.
  Nothing in `engram-models` or below may reference `Situation` scoring
  config consumers above it; conflict routing lives in `engram-models`
  (nli/challenge.rs) exactly like today.
- **One schema bump**: all seven new columns (§4.1) land in a single
  `CURRENT_SCHEMA_VERSION` 0.2.0 → 0.3.0 change in increment I2. No other
  increment touches the manifest version.
- **Each increment is one reviewable PR** on a stacked branch off
  `claude/memory-config-analysis-io11yc` (or sequential merges — owner's
  choice). Behavior changes are inert until their consumer lands (e.g. the
  index columns exist before anything reads them).

## 1. Increment overview & dependency order

| Inc | Title | Crates touched | Depends on | Size |
|-----|-------|----------------|------------|------|
| I1 | Domain types + config types | engram-types **+ mechanical `Memory`-literal updates workspace-wide** | — | M |
| I2 | File format + index schema 0.3.0 | engram-storage (+ fuzz) | I1 | M |
| I3 | Scoring + retrieval plumbing (incl. exclusion predicate threading) | core (scoring, retrieval, filters), fuzz | I2 | M |
| I4 | Ops: create/update/resolve/verify/gc/compress/doctor + conflict routing | core (ops), engram-models, engram-storage (write helpers) | I3 | L |
| I5a | MCP + CLI surfaces (create/query/update/resolve/verify + suffixes for `verify`) | engram-mcp, engram-cli | I4 | M |
| I5b | Task tools (`task_current`/`task_complete` MCP+CLI + their suffixes) | engram-mcp, engram-cli | **I7** | S |
| I6 | Hooks: rendering + four new events (session-end = flush-only here) | engram-cli | I4 (I5a for shared parsing helpers) | **L** |
| I7 | Lifecycle: tasks, demotion, promotion, consolidation (+ telemetry recording prerequisite) | core (ops, ops/maintenance), engram-storage (telemetry) | I4 | L |
| I8 | Teaching & packaging: ENGRAM.md, plugin.json hook manifests, docs | engram-cli (setup), .claude-plugin | I5b, I6 | S |

I5a and I6 can proceed in parallel once I4 merges; I7 in parallel with both,
**except** that I6's session-end demotion wiring and the §16.4 hint line are
deferred to I7/I5b respectively (see increment details — I6 alone ships
flush-only session-end and no hint line). I8 is last (it documents and wires
the shipped surface).

**`MCP_TOOL_SUFFIXES` moves with its tools** (spec §5.5 pinned-test rule):
`verify` lands in I5a's PR, `task_current`/`task_complete` in I5b's — never
deferred to I8, or the pinned `mcp_tool_suffixes_match_server_tools`
equality test fails every intermediate increment's workspace gate.

**Invariant → increment homes** (spec §14): 1→I1, 2→I2, 3→I3, 4→I3, 5→I3,
6→I3 (structural no-age-terms half) + I7 (demotion-replaces-curve half),
7→I4, 8→I4, 9→I6, 10→I2+I3, 11→I7, 12→every increment, 13→I4, 14→I2
(index exclusion) + I3 (NLI candidates, query threading) + I4 (atomic
closure) + I6 (hook surfaces), 15→I5a/I5b (suffixes) + I6 (stdin safety),
16→I8.

## 2. Increment detail

### I1 — Domain types + config types (`crates/engram-types`)

Spec: §2 (all), §7.1–7.2 config shapes, §12.

New files:
- `src/epistemic.rs`: `Epistemic` (Copy/Eq/Hash, serde lowercase,
  `Display`/`FromStr`), `Generality` (Default = Project), `Validity`
  (+ `is_empty()`), `Situation` enum (Copy/Eq, serde lowercase — lives in
  types because both core scoring and CLI hooks name it).

Modified files:
- `src/memory.rs`:
  - `Memory` fields: `epistemic: Epistemic`, `valid_while: Option<Validity>`,
    `valid_from`, `invalidated_at`, `superseded_by` (§2.3, §2.4).
  - `MemoryType::default_epistemic()` (§2.5 mapping, D3: Hazard → Fact).
  - Free fn `default_decay(MemoryType, Epistemic) -> Option<Decay>` with the
    diagonal invariant (§2.6); `MemoryType::default_decay()` untouched.
  - `Memory::new` initialization; `is_invalidated_at(now)`; `is_active()`
    extended; `MemoryUpdate` fields + `apply_to` arms (all-empty `Validity`
    clears to `None`; explicit `invalidated_at`/`superseded_by` clearing
    supported for §2.4 reopening).
- **Workspace-wide mechanical updates**: adding required fields to `Memory`
  breaks every struct-literal construction site under the workspace clippy
  gate — production code (`parse_v2`'s `Ok(Memory { ... })` in
  engram-storage) and test literals across core, engram-cli, and
  engram-storage. I1 carries all of these mechanically (fields set to
  defaults). The "crates touched" cell reflects this.
- `src/config.rs`:
  - `SituationConfig { floor, session_start, file_edit, debugging,
    design_choice }` with `SituationProfile { fact, observation, decision }`;
    defaults per §7.1 table; `validate()` ([0,1] checks) wired where
    `ScoringWeights::validate` is called; non-finite clamps to neutral on
    read.
  - `ScoringConfig.challenge_penalty: ChallengePenalty` untagged enum
    (`Flat(f64)` | `PerClass{..}`), default PerClass{0.15, 0.20, 0.05};
    accessor `penalty_for(Epistemic) -> f64` with [0,1] clamp (§7.2).
  - `EpistemicConfig` (§12: review days, half-life/floor, demote/promote/
    consolidate knobs, `invalidated_retention_days`), default section.
  - **New** `HooksConfig` struct + `[hooks]` section + `EngramConfig.hooks`
    field (none exists today — this is a new config surface, not an
    addition): `class_order: Option<Vec<String>>`,
    `prompt_context_budget: usize` (default 1000).
- `src/lib.rs`: re-exports.

Notes: `engram-types` is the fast-iteration crate (`cargo check -p
engram-types` in seconds) — land the complete type surface here even for
fields consumed only in later increments, so downstream increments never
re-touch types.

### I2 — File format + index schema (`crates/engram-storage`, `fuzz`)

Spec: §3, §4, §2.4 (persistence side).

- `src/memory_file/v2.rs`: `MinimalFrontmatter.epistemic: Option<Epistemic>`;
  `HiddenMeta` + `valid_while`, `valid_from`, `invalidated_at`,
  `superseded_by`. Writer: off-diagonal-only emission of `epistemic`;
  `Some`-and-non-empty emission of `valid_while`; parser materialization per
  §3.1. `src/memory_file/v1.rs`: defaulting only (§3.2).
- `src/lance_index.rs`: seven new columns (§4.1 table: `epistemic`,
  `verified_at`, `generality`, `origin_task`, `valid_from`,
  `invalidated_at`, `watch_paths`); `IndexForFiltering` gains the §4.2
  field set (incl. `invalidated_at`, `watch_paths`), the displayable
  projection carries `valid_from`; row build/read from `Memory`.
  `watch_paths` follows the existing multi-value convention:
  serde_json-encoded Utf8 column (like `physical`/`tags`), glob-matched in
  Rust — no Arrow List type. **Note:** the default-exclusion *predicate*
  lives in core (`src/retrieval/filters.rs`, next to the expiry predicate)
  and lands in I3 — I2 only provides the columns and projections. Harmless
  ordering: no writer sets `invalidated_at` until I4.
- `src/manifest.rs`: `CURRENT_SCHEMA_VERSION = "0.3.0"`. Migration is the
  existing reindex-on-open; add a fixture-based test (store stamped 0.2.0
  with pre-epistemic files → open → columns present, values materialized
  from the §2.5 defaults, vectors preserved).
- `src/store.rs`: write path passes the new fields through; window-closure
  helper `MemoryStore::invalidate_with(id, superseded_by, now)` built on
  `update_with` (used by ops in I4).
- `fuzz/`: extend `memory_file` / `memory_file_roundtrip` input coverage
  notes (structs carry the fields automatically); commit any new crash
  artifacts as corpus seeds per repo policy.

### I3 — Scoring + retrieval plumbing (core, `fuzz`)

Spec: §6, §7, §4.2.

- `src/scoring/composite.rs`:
  - `ScoreTarget` + `epistemic`, `verified_at`; `From<&Memory>` updated.
  - Situation multiplier after trust (§7.1), `ScoringContext.situation`,
    `ScoreBreakdown.situation_multiplier`.
  - Per-class challenge penalty via `config.penalty_for(target.epistemic)`.
  - Fact freshness anchor (§7.3): `anchor = verified_at.unwrap_or(created_at)`
    for `Fact` only.
- `src/retrieval/engine.rs` + `filters.rs`: `RetrievalQuery.epistemic`,
  `.include_invalidated`, `.situation`; index-level epistemic filter beside
  the `types` filter; the **default-exclusion predicate**
  `invalidated_at IS NULL OR invalidated_at > <now>` added in
  `build_filter_predicate` next to the existing `expires_at` predicate
  (same shape — the precedent at filters.rs:195 makes this trivial);
  situation threaded into every `ScoringContext` construction site (all four
  modes). NLI ingestion candidate set excludes invalidated memories (§14.14).
- `src/retrieval/engine.rs` Step 8 comment: note rerank sees post-situation
  survivors (§7.1 interactions).
- `fuzz/fuzz_targets/composite_score.rs`: add epistemic discriminant (u8 →
  class), situation selector (u8 → Option<Situation>), per-class penalty
  f64s; same finiteness assertion; register nothing new in `fuzz/Cargo.toml`
  (existing `[[bin]]`).

Parity requirement (§4.2): `composite_score` ≡ `composite_score_target` —
the test-plan's paired test must land in this increment, not later.

### I4 — Ops + conflict routing (core `ops`, `engram-models`)

Spec: §5.1, §9, §10, §2.4 write paths.

- `src/ops/create.rs`: `CreateParams` new fields (§5.1); assembly of
  `valid_while`; `default_decay(type_, epistemic)` when no explicit decay;
  **supersedes closes windows** — after the new memory persists, iterate
  `supersedes` ids via `MemoryStore::invalidate_with` (skip-and-log missing,
  §2.4.1). Same for `ops::update` when an update adds supersedes entries.
- `src/ops/resolve.rs` (confirmed: `ResolveAction { Keep, Update, Delete }`
  + `resolve_memory`): add an `Invalidate` arm — close window, optional
  `superseded_by`, keep file (§5.5). (Not to be confused with
  `src/daemon/resolve.rs`, which is provider resolution.)
- New `src/ops/verify.rs`: §10.4 (`verified_at = now`; conditional
  NeedsReview → Active reset when the pending review finding came from
  §10.1/§10.3 — encode the origin in the challenge/finding record written by
  doctor `--fix`).
- `src/ops/gc.rs`: retention rule for invalidated memories
  (`invalidated_retention_days`, dry-run first, §2.4).
- `src/ops/compress.rs`: `compress_apply` invalidates sources
  (`superseded_by = summary id`) instead of deleting (§2.4.3);
  `skipped_sources` semantics preserved for already-gone ids.
- `src/ops/doctor.rs`: three checks (§10.1 invalidated-path via mtime,
  §10.2 stale-observation, §10.3 derived-from cascade); report-only default,
  `--fix` flips per spec (E4); findings carry machine-readable origin tags
  for `verify` to consume.
- `src/ops/parsing.rs`: `parse_epistemic`, `parse_generality`,
  `parse_situation` (string → enum, mirroring `parse_memory_type`).
- `ops::update` params: thread `epistemic`/`valid_while`/`valid_from` and
  the reopening path (clear `invalidated_at`+`superseded_by`) through the
  update op — the `MemoryUpdate` arms exist from I1; this increment wires
  the ops-level params that I5a's `--clear-invalidated`/`clear_invalidated`
  surfaces call.
- `crates/engram-models/src/nli/challenge.rs`: routing table + entrenchment
  (§9.1–9.2). Signature change: `challenge_for_contradictions(store,
  new_memory: &NewMemoryMeta, contradictions)` where `NewMemoryMeta` carries
  id/summary/epistemic/provenance/confidence/verified-or-created timestamp.
  (DAG note: passing `&Memory` directly would be equally clean —
  challenge.rs already imports `Memory` from engram-types, a leaf crate —
  the smaller projection is preferred for testability, not required for
  layering.) Do the signature change and all call sites (engine ingestion
  tail, `ops::challenge` re-export) in one commit to keep `-D warnings`
  green. These routing tests are **model-free**: the function takes plain
  `(String, NliResult)` data, so synthetic `NliResult`s cover the full
  matrix without loading NLI.

### I5a — MCP + CLI surfaces (`engram-mcp`, `engram-cli`)

Spec: §5.2–5.5 (minus task tools), §6, §16.2 (descriptions land here;
ENGRAM.md text in I8).

- `crates/engram-mcp/src/server.rs`:
  - `CreateInput`/`UpdateInput`: `epistemic`, `premise`, `invalidated_by`,
    `origin_task`, `generality`, `valid_from` (+ update-side clearing:
    `clear_validity`, `clear_invalidated`);
    `QueryInput`: `epistemic`, `situation`, `include_invalidated`;
    `ListInput`: `include_invalidated`.
  - New tool `verify` (§5.5); `resolve` invalidate action; description
    updates per §16.2. **`MCP_TOOL_SUFFIXES` gains `verify` in this PR**
    (pinned-test rule).
  - `ScoreBreakdownOutput` gains `situation_multiplier` (spec §7.1
    observability — fields are hand-enumerated in server.rs, so this is an
    explicit addition, and the walkthrough's spot-check depends on it).
- `crates/engram-cli/`: `app.rs` clap definitions (`--epistemic`,
  `--premise`, `--invalidated-by`, `--origin-task`, `--generality`,
  `--valid-from`, `--situation`, `--include-invalidated`,
  `--clear-validity`, `--clear-invalidated`; `verify` subcommand);
  `commands/{create,update,query,list,resolve}.rs` threading; new
  `commands/verify.rs`; `output.rs` class/invalidated tags (§5.4).

### I5b — Task tools (`engram-mcp`, `engram-cli`) — after I7

Spec: §5.5 task rows, §16.4 hint precondition.

- MCP `task_current`/`task_complete` wired to I7's ops; CLI `task
  current`/`task complete` subcommands (`commands/task.rs`).
- **`MCP_TOOL_SUFFIXES` gains `task_current`/`task_complete` in this PR.**
- The §16.4 session-start hint line ships here too (it advertises
  `task_current`; spec forbids advertising an uninstalled tool). No stubs
  ever — if this increment hasn't shipped, the tools simply don't exist
  (risk 6 policy).

### I6 — Hooks (`engram-cli`)

Spec: §8 (all), §16.4.

- `commands/hook.rs`:
  - Grouping by class, per-class rendering, decision atomicity in
    `format_detailed_context_with_budget`, task-generality *suppression*
    from index columns (the un-suppress branch — matching the session's
    declared task — needs I7's mapping reader and lands with I7's wiring).
    Situation wiring (SessionStart/FileEdit). Hint line: **not here** —
    ships in I5b (§16.4 precondition).
  - New subcommands: `user-prompt-submit` (prompt→Filter query + §8.5.1
    situation inference keyword tables as `const` lists; budget from
    `prompt_context_budget`), `post-tool-use` (§8.5.2 `watch_paths` match
    with the §2.4 validity guard), `session-end` (**flush-only in this
    increment**: telemetry flush; task-mapping clear + optional demotion
    wire in with I7), `pre-compact` (§8.5.4 static reminder; verify
    additionalContext support during implementation, else no-op).
  - All new subcommands: exit 0 + empty stdout on malformed/empty stdin
    (§14.15). **Engine construction**: like the existing hooks, use
    `build_engine_without_providers` — hooks today deliberately skip
    provider resolution entirely (no daemon policy involved), and
    `DaemonPolicy::ConnectOnly` would fall back to *in-process model
    loading* when no daemon runs, which hooks must never do. So
    user-prompt-submit is keyword-only retrieval by default; a
    "connect-if-live, else no providers" resolution mode is an optional
    later enhancement for semantic hook retrieval, not part of this
    increment.
- `app.rs`: clap variants for the four events.

### I7 — Lifecycle (core, storage telemetry)

Spec: §11 (all).

- **Telemetry prerequisite (promotion's data does not exist yet):** today's
  collector records query-level outcomes only (`record_query_outcome(hit,
  quality, session_id)` — no memory ids). I7 adds (a) a recording path in
  the engine emitting retrieved-memory ids for above-threshold results, and
  (b) a `stats_events` extension carrying them. `stats_events` is created
  via `ensure_table` and is **not** versioned by the manifest bump, so the
  extension must be append-compatible with existing tables (nullable new
  column + tolerant reader) or carry its own one-shot rebuild — decide at
  implementation; either way it is outside the one-manifest-bump rule,
  which governs the memories table only.
- Session→task mapping: small JSON under the project's `.engramdb/` state
  dir keyed by provenance session id (§11.1); helpers in `engram-storage`
  (paths + atomic write, same temp-then-rename discipline).
- `src/ops/task.rs`: `task_current` (read/write mapping), `task_complete`
  (§11.2 demotion under write lock; project-generality notices).
- Hook wiring deferred from I6: session-end task-mapping clear + configured
  demotion; the hook-side un-suppress branch of §8.3 gating.
- Promotion (§11.3): maintenance-pass job counting distinct
  above-threshold retrieval sessions from the new telemetry data; doctor
  suggestion; auto path behind `auto_promote`.
- Consolidation (§11.4): extend `ops::compress` with the observation-cluster
  pass (embedding similarity + pairwise-NLI gate); suggestion-first; sources
  demoted via decay flip; `derived_from` written on the new Fact.
  Provider-dependent steps skip gracefully with a logged notice when no
  providers resolve (§14.11).
- Wiring: jobs registered in the existing throttled maintenance pass
  (`src/ops/maintenance.rs`, `MaintenanceConfig` cadence — note: there is
  no `daemon/maintenance` module; §11.5's daemon-resident idle scheduling
  is explicitly deferred, the maintenance pass host is `ops`).

### I8 — Teaching & packaging (`engram-cli` setup, `.claude-plugin`, docs)

Spec: §16 (all), §14.15–16.

- `setup.rs`: `ENGRAM_MD_CONTENT` rewrite (§16.1 text verbatim); four new
  hook entries in the settings.json fallback writer; lockstep test parsing
  `.claude-plugin/plugin.json` and comparing hook event sets (§16.3).
  (`MCP_TOOL_SUFFIXES` was already updated in I5a/I5b with its tools.)
- `.claude-plugin/plugin.json`: version bump, description sentence, §8.5
  hook entries (PostToolUse matcher `Write|Edit|MultiEdit`).
- Docs: `docs/contributors/architecture.md` (epistemic axis + bi-temporal
  model section), `.claude/CLAUDE.md` fuzzing/section touch-ups only if
  commands changed, release-notes draft calling out the D4 behavior change
  and the one-line `floor = 1.0` opt-out (§13 note).

## 3. Sequencing risks & mitigations

1. **Schema-bump collision with other in-flight work**: I2 must merge before
   any other branch bumps the manifest; coordinate on the 0.3.0 number.
2. **`challenge_for_contradictions` signature change** (I4) ripples to the
   engine ingestion tail and `ops::challenge` re-export — do the signature
   and all call sites in one commit to keep `-D warnings` green.
3. **Hook latency**: §8.5.1/8.5.2 must stay index-only with
   `build_engine_without_providers` (see I6 — no daemon policy, no model
   loads); the test plan carries a latency budget check (release build,
   warm cache). If UserPromptSubmit keyword-search latency is unacceptable
   on large stores, degrade to summary-index keyword match only — decision
   recorded in code comment, not config.
4. **PreCompact API uncertainty** (§8.5.4): confirm additionalContext
   support against the Claude Code hooks docs at I6 time; fallback is a
   no-op subcommand (still registered, so manifests stay in lockstep).
5. **Sandbox model staging**: I4's routing tests are model-free (synthetic
   `NliResult`s — see I4). Only I7's *optional* real-model consolidation
   E2E touches models; prefer stub providers (`StubEmbeddingProvider`,
   synthetic NLI) for the gate logic, and if a real-model test is added,
   register it explicitly in `.config/nextest.toml`'s `ml-models` filter
   (membership is a hand-written filter, not automatic).
6. **Task-tool sequencing**: resolved structurally by the I5a/I5b split —
   task tools and their suffixes ship only after I7, never stubbed.

## 4. Estimated review checkpoints

- After I3: the scoring behavior freeze — run §14.3–14.5 plus the
  structural (no-age-terms) half of §14.6; any formula disagreement is
  cheapest to fix here. (§14.6's demotion-replaces-curve half runs at I7.)
- After I6: hook rendering walkthrough via **manual stdin invocation**
  (`echo '<event json>' | engramdb hook <event>`) — live Claude Code wiring
  doesn't exist until I8's manifests.
- After I7 (+I5b): the full end-to-end walkthrough from the test plan §2.1,
  including task lifecycle steps.
- After I8: fresh `engramdb setup` in a clean project; verify plugin +
  settings.json parity, permission-prompt-free tool calls, ENGRAM.md content.
