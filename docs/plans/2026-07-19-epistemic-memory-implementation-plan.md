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
| I1 | Domain types + config types | engram-types | — | M |
| I2 | File format + index schema 0.3.0 | engram-storage (+ fuzz) | I1 | M |
| I3 | Scoring + retrieval plumbing | core (scoring, retrieval), fuzz | I2 | M |
| I4 | Ops: create/update/resolve/verify/gc/compress/doctor + conflict routing | core (ops), engram-models, engram-storage (write helpers) | I3 | L |
| I5 | MCP + CLI surfaces | engram-mcp, engram-cli | I4 | M |
| I6 | Hooks: rendering + four new events | engram-cli | I4 (I5 for shared parsing helpers) | M |
| I7 | Lifecycle: tasks, demotion, promotion, consolidation | core (ops, daemon/maintenance), engram-storage (telemetry) | I4 | L |
| I8 | Teaching & packaging: ENGRAM.md, tool suffixes, plugin.json, docs | engram-cli (setup), .claude-plugin | I5, I6 | S |

I5 and I6 can proceed in parallel once I4 merges. I7 is independent of I5/I6
and can run in parallel with them. I8 is last (it documents shipped surface).

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
  - `HooksConfig` additions: `class_order: Option<Vec<String>>`,
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
  `invalidated_at`, `watch_paths`); `IndexForFiltering` + the
  filterable/displayable projections gain matching fields; row build/read
  from `Memory`. Default-exclusion predicate `invalidated_at IS NULL`
  applied in the same layer as the expiry filter, behind an
  `include_invalidated` flag threaded from the query (§6.1).
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
  the `types` filter; invalidated exclusion threaded to the I2 predicate;
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
- `src/ops/resolve.rs` (or the existing resolve module): `invalidate` action
  — close window, optional `superseded_by`, keep file (§5.5).
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
- `crates/engram-models/src/nli/challenge.rs`: routing table + entrenchment
  (§9.1–9.2). Signature change: `challenge_for_contradictions(store,
  new_memory: &NewMemoryMeta, contradictions)` where `NewMemoryMeta` carries
  id/summary/epistemic/provenance/confidence/verified-or-created timestamp
  (a small struct in engram-models, built by the engine caller — keeps the
  DAG clean; engram-models still sees no core types). `ops::challenge`
  re-export stays stable.

### I5 — MCP + CLI surfaces (`engram-mcp`, `engram-cli`)

Spec: §5.2–5.5, §6, §16.2 (descriptions land here; ENGRAM.md text in I8).

- `crates/engram-mcp/src/server.rs`:
  - `CreateInput`/`UpdateInput`: `epistemic`, `premise`, `invalidated_by`,
    `origin_task`, `generality` (+ update-side clearing semantics);
    `QueryInput`: `epistemic`, `situation`, `include_invalidated`;
    `ListInput`: `include_invalidated`.
  - New tools `verify`, `task_current`, `task_complete` (§5.5 table;
    `task_*` may land as stubs returning a clear "lifecycle ships in I7"
    error if I7 hasn't merged — or I5 waits on I7; owner's sequencing call.
    Default plan: land them wired to the I7 ops, i.e. I5's task tools merge
    after I7's ops exist. `verify` has no such dependency).
  - `resolve` invalidate action; description updates per §16.2.
- `crates/engram-cli/`: `app.rs` clap definitions (`--epistemic`,
  `--premise`, `--invalidated-by`, `--origin-task`, `--generality`,
  `--situation`, `--include-invalidated`, `--clear-validity`; `verify`,
  `task` subcommands); `commands/{create,update,query,list,resolve}.rs`
  threading; new `commands/{verify,task}.rs`; `output.rs` class/invalidated
  tags (§5.4).

### I6 — Hooks (`engram-cli`)

Spec: §8 (all), §16.4.

- `commands/hook.rs`:
  - Grouping by class, per-class rendering, decision atomicity in
    `format_detailed_context_with_budget`, task-generality gating from index
    columns, situation wiring (SessionStart/FileEdit), hint line (§16.4).
  - New subcommands: `user-prompt-submit` (prompt→Filter query + §8.5.1
    situation inference keyword tables as `const` lists; budget from
    `prompt_context_budget`), `post-tool-use` (§8.5.2 `watch_paths` match),
    `session-end` (§8.5.3 telemetry flush + optional demotion), `pre-compact`
    (§8.5.4 static reminder; verify additionalContext support during
    implementation, else no-op).
  - All new subcommands: exit 0 + empty stdout on malformed/empty stdin
    (§14.15); `ConnectOnly` daemon policy like existing hooks; no in-process
    model loads.
- `app.rs`: clap variants for the four events.

### I7 — Lifecycle (core, storage telemetry)

Spec: §11 (all).

- Session→task mapping: small JSON under the project's `.engramdb/` state
  dir keyed by provenance session id (§11.1); helpers in `engram-storage`
  (paths + atomic write, same temp-then-rename discipline).
- `src/ops/task.rs`: `task_current` (read/write mapping), `task_complete`
  (§11.2 demotion under write lock; project-generality notices).
- Promotion (§11.3): maintenance-pass job counting distinct
  above-threshold retrieval sessions from telemetry; doctor suggestion; auto
  path behind `auto_promote`.
- Consolidation (§11.4): extend `ops::compress` with the observation-cluster
  pass (embedding similarity + pairwise-NLI gate); suggestion-first; sources
  demoted via decay flip; `derived_from` written on the new Fact.
  Provider-dependent steps skip gracefully with a logged notice when no
  providers resolve (§14.11).
- Wiring: jobs registered in the existing throttled maintenance pass
  (`MaintenanceConfig` cadence); SessionEnd hook calls into `task` ops when
  configured (§8.5.3).

### I8 — Teaching & packaging (`engram-cli` setup, `.claude-plugin`, docs)

Spec: §16 (all), §14.15–16.

- `setup.rs`: `ENGRAM_MD_CONTENT` rewrite (§16.1 text verbatim);
  `MCP_TOOL_SUFFIXES` += `verify`, `task_current`, `task_complete`; four new
  hook entries in the settings.json fallback writer; lockstep test parsing
  `.claude-plugin/plugin.json` and comparing hook event sets (§16.3).
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
3. **Hook latency**: §8.5.1/8.5.2 must stay index-only + daemon-ConnectOnly;
   the test plan carries a latency budget check. If UserPromptSubmit
   keyword-search latency is unacceptable on large stores, degrade to
   summary-index keyword match only (no embedding call) — decision recorded
   in code comment, not config.
4. **PreCompact API uncertainty** (§8.5.4): confirm additionalContext
   support against the Claude Code hooks docs at I6 time; fallback is a
   no-op subcommand (still registered, so manifests stay in lockstep).
5. **Sandbox model staging**: I4 (NLI routing) and I7 (consolidation) tests
   touch `ml-models`-group tests — keep them in the nextest group
   (`max-threads = 1`) and respect the existing flaky-test caveats.
6. **Task-tool sequencing** (I5 vs I7): default is I7 before I5's task
   tools; if I5 ships first, `task_current`/`task_complete` are omitted from
   that release (not stubbed) to avoid advertising dead tools — and
   `MCP_TOOL_SUFFIXES` additions move with them.

## 4. Estimated review checkpoints

- After I3: the scoring behavior freeze — run the full §14.3–14.6 invariant
  suite; any formula disagreement is cheapest to fix here.
- After I6: end-to-end hook walkthrough in a scratch project (manual
  validation script in the test plan).
- After I8: fresh `engramdb setup` in a clean project; verify plugin +
  settings.json parity, permission-prompt-free tool calls, ENGRAM.md content.
