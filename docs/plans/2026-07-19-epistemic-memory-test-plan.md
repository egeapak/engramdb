# Validation & test plan: Epistemic memory classes

Companion to `2026-07-19-epistemic-memory-spec.md` (§14 invariants are the
contract) and `2026-07-19-epistemic-memory-implementation-plan.md` (Ix
increment references). Test names follow existing repo conventions
(`test_<subject>_<behavior>`); every test lands in the same increment as the
code it covers unless noted.

## 1. Unit tests by area

### 1.1 Types (`engram-types`, I1)

| Test | Asserts | Spec |
|------|---------|------|
| `test_default_epistemic_mapping` | all 8 `MemoryType` → class values, incl. Hazard→Fact (D3) | §2.5 |
| `test_default_decay_diagonal_invariant` | `default_decay(t, t.default_epistemic()) == t.default_decay()` for all 8 types (paired-default style, like the NLI repo-drift tests) | §14.1 |
| `test_default_decay_off_diagonal` | Observation off-diagonal → exp 90d floor 0.2; Fact → none; Decision → type default | §2.6 |
| `test_validity_is_empty` | all-empty ⇒ true; each single field ⇒ false | §2.2 |
| `test_memory_new_epistemic_defaults` | `Memory::new` sets diagonal class, `valid_while: None`, `valid_from`/`invalidated_at`/`superseded_by` None (`verified_at` already exists and keeps its own tests — don't re-cover) | §2.3 |
| `test_memory_update_apply_epistemic_and_validity` | update arms incl. all-empty-Validity-clears and invalidated_at set/clear (reopening) | §2.3, §2.4 |
| `test_is_active_invalidated` | `invalidated_at` past ⇒ not active; future ⇒ active; None ⇒ unchanged behavior | §2.4 |
| `test_situation_config_defaults_and_validate` | §7.1 table values; floor/profile ∈ [0,1] rejection; serde-default section absent ⇒ defaults; snake_case wire values (`design_choice`, not `designchoice`) | §7.1, §12, §6.2 |
| `test_situation_profile_structure_no_age_terms` | `SituationProfile` is exactly three `f64` class fields (structural half of invariant 6 — a compile-anchored field-count/type assertion) | §14.6 |
| `test_challenge_penalty_untagged_compat` | `challenge_penalty = 0.10` parses as Flat and `penalty_for(*) == 0.10`; table form parses PerClass; defaults 0.15/0.20/0.05; clamp on read | §7.2, §14.4 |
| `test_epistemic_config_defaults` | §12 `[epistemic]` defaults incl. `invalidated_retention_days = 180` | §12 |

### 1.2 File format (`engram-storage::memory_file`, I2)

| Test | Asserts | Spec |
|------|---------|------|
| `test_v2_roundtrip_off_diagonal_epistemic` | write→parse identity; `epistemic` key present only when off-diagonal | §3.1, §14.2 |
| `test_v2_roundtrip_validity_and_bitemporal` | true cross-product: each class × each `Validity` field-population combination (incl. sparse: only `origin_task`; only `premise`) × bi-temporal set/unset; empty Validity not emitted | §3.1, §14.2 |
| `test_v2_parse_pre_epistemic_file` | inline-const fixture (repo convention — no `fixtures/` dir; existing format tests are inline literals) written by today's writer parses with §2.5 defaults, `valid_while: None` | §14.2 |
| `test_v1_parse_defaults_epistemic` | V1 path defaults from type | §3.2 |
| `test_v2_diagonal_writes_byte_identical` | a diagonal memory with no new fields produces byte-identical output to a golden inline const. Preconditions: (a) pin all timestamps by overwriting after `Memory::new` (which stamps now; `write_v2` itself is deterministic — no clock reads); (b) capture the golden bytes from **pre-I2 master** and check them in before I2 lands; (c) understand the failure mode — this test enforces `skip_serializing_if` on every new field | §3.1 |

Fuzz: `memory_file` / `memory_file_roundtrip` run ≥ 60s locally after I2
(`cargo +nightly fuzz run memory_file -- -max_total_time=60`); any crash
input committed as corpus seed (repo policy).

### 1.3 Index & migration (`engram-storage`, I2)

| Test | Asserts | Spec |
|------|---------|------|
| `test_index_row_carries_new_columns` | all 7 columns written/read for a fully-populated memory AND sparse variants (only `origin_task`; `invalidated_at` without `superseded_by`; `valid_while: None` ⇒ `generality` default) — nullable-column materialization is where bugs live; `watch_paths` mirrors `valid_while.invalidated_by` as JSON-in-Utf8 | §4.1 |
| `test_migration_0_2_0_to_0_3_0` | repo convention (mirror `schema_migration_on_open_backfills_decay_and_has_embedding`): build store in TempDir with current code, rewrite `manifest.schema_version` to 0.2.0, reopen → 0.3.0, columns materialized per defaults, `has_embedding`/vectors preserved. No checked-in binary store fixture | §4.1, §13 |
| `test_new_store_born_current` | fresh init stamps 0.3.0 directly | §4.1 |
| `test_default_query_excludes_invalidated` | index predicate excludes past `invalidated_at`; **future-dated `invalidated_at` is included** (predicate is `IS NULL OR > now`, §2.4); `include_invalidated` includes; expiry behavior untouched; asserted in **both** Rank and Filter modes | §2.4, §14.14 |
| `test_invalidate_with_atomic` | `invalidate_with` closes window under write lock; concurrent challenge/edit survives (mirror `test_update_with_serializes_concurrent_updates`) | §2.4, §14.14 |

### 1.4 Scoring (`src/scoring`, I3)

| Test | Asserts | Spec |
|------|---------|------|
| `test_situation_none_scores_unchanged` | `situation: None` ⇒ breakdown identical to pre-change values in all four weight modes for **unchallenged** memories (golden values from existing tests) | §14.4 |
| `test_challenged_default_penalty_shift_intended` | under default (PerClass) config, a challenged fact/observation/decision shifts by exactly the intended §13-②  delta vs old flat 0.10; under `Flat(0.10)` config, identical to pre-change for all classes | §13, §14.4 |
| `test_situation_multiplier_floor_transform` | mult = `floor + (1-floor)*p` for each class × each default profile; breakdown records it | §7.1 |
| `test_situation_worst_case_above_threshold` | exact-scope, crit 0.8, human, unchallenged ⇒ ≥ 0.45 under every default profile value (mirror `test_trust_floor_prevents_extreme_compounding`) | §14.5 |
| `test_situation_nonfinite_config_neutral` | NaN/inf floor or profile ⇒ multiplier 1.0 | §7.1 |
| `test_challenge_penalty_per_class` | challenged fact/-obs/-decision lose exactly 0.15/0.20/0.05; Flat config loses flat value for all | §7.2 |
| `test_fact_freshness_anchor_verified_at` | Fact with decay curve + old created_at + recent verified_at scores as fresh; Observation/Decision anchor unchanged | §7.3 |
| `test_score_target_parity_new_fields` | `composite_score` ≡ `composite_score_target` parameterized over each class × `verified_at` None/Some (extend existing parity coverage) | §14.3 |
| `test_situation_applied_pre_rerank` | with a stub reranker, situation multiplier affects which candidates survive to rerank (ordering claim of §7.1, not just a comment) | §7.1 |

Fuzz: extended `composite_score` target (epistemic discriminant, situation
selector, raw-f64 penalties) — finiteness assertion; ≥ 60s local run at I3.

### 1.5 Retrieval engine (`src/retrieval`, I3)

| Test | Asserts | Spec |
|------|---------|------|
| `test_query_epistemic_filter` | hard filter narrows like `types`, both modes | §6.1 |
| `test_query_include_invalidated` | default excludes; flag includes and result carries invalidated tag data | §6.1 |
| `test_situation_threaded_all_modes` | situation reaches `ScoringContext` in keyword/semantic/degraded/scope-only paths | §6.2 |
| `test_nli_candidates_exclude_invalidated` | ingestion contradiction pass never targets an invalidated memory | §14.14 |

### 1.6 Ops & conflict (`src/ops`, `engram-models`, I4)

| Test | Asserts | Spec |
|------|---------|------|
| `test_create_assembles_validity` | params → `valid_while` (None when all empty); class defaulting; two-arg decay default; explicit decay wins | §5.1 |
| `test_create_supersedes_closes_windows` | referenced live memories get `invalidated_at`+`superseded_by`; missing ids skipped+logged; op succeeds | §2.4.1 |
| `test_update_supersedes_closes_windows` | same via update | §2.4.1 |
| `test_resolve_invalidate_action` | closes window, keeps file, optional superseded_by | §5.5 |
| `test_reopen_restores_default_retrieval` | invalidate → default query absent → update clears `invalidated_at`/`superseded_by` → **index row refreshed, memory present in default query again** (end-to-end reversibility) | §2.4 |
| `test_valid_from_set_and_rendered` | create with `valid_from` persists it; displayable projection and json output carry it | §2.4, §5.1, §5.4 |
| `test_verify_stamps_and_reopens` | `verified_at = now`; NeedsReview→Active only for doctor-originated findings (origin tag respected) | §10.4 |
| `test_gc_invalidated_retention` | dry-run lists, real run purges only past retention; `0` keeps forever | §2.4 |
| `test_compress_apply_invalidates_sources` | sources invalidated not deleted; `superseded_by` = summary id; `skipped_sources` for already-gone | §2.4.3 |
| `test_doctor_invalidated_path_check` | watched-glob mtime newer than anchor ⇒ finding; `--fix` flips NeedsReview. Mtime control via `File::set_modified` — never sleep-based or same-second creation (flakiness trap) | §10.1 |
| `test_doctor_stale_observation_check` | age > threshold ⇒ finding; never flips even with `--fix` | §10.2 |
| `test_doctor_derived_from_cascade` | missing/challenged source ⇒ finding; one level only | §10.3 |
| `test_doctor_plain_run_readonly` | one store triggering **all three** findings simultaneously: plain `doctor` run ⇒ `.engramdb/memories/*.md` byte-identical before/after (hash the memory files only — opening the store legitimately touches manifest/LanceDB internals) | §14.8 |
| `test_challenge_routing_matrix` | the 7 fully-typed (new × existing) cells produce the §9.1 outcomes; decision-vs-decision sets NeedsReview never Challenged (§14.7). Model-free: inject synthetic `NliResult`s (the fn takes plain data) | §9.1 |
| `test_challenge_entrenchment_tiebreak` | same-class **fact/observation** conflicts with controlled trust/recency/confidence inputs: direction follows §9.2 order at each tier; plus negative assertion that decision-vs-decision ignores entrenchment (always NeedsReview-on-existing) | §9.2 |
| `test_no_routing_path_deletes` | file count unchanged across every routing outcome; also asserted on the supersedes-closure path (§14.13) | §14.7, §14.13 |

### 1.7 MCP + CLI (`engram-mcp`, `engram-cli`, I5)

| Test | Asserts | Spec |
|------|---------|------|
| `test_mcp_create_epistemic_params` | new CreateInput fields parse and reach ops — on stores/queries that never require an embedding (avoid the in-process-ONNX flake; daemon path is compiled out under `#[cfg(test)]`) | §5.2 |
| `test_mcp_update_epistemic_params` | UpdateInput fields incl. `clear_validity`/`clear_invalidated` reach ops; clearing semantics end-to-end | §5.2, §2.4 |
| `test_mcp_invalid_enum_errors` | table test: invalid `epistemic` (create+update), invalid `situation`, invalid `generality`, invalid entry inside the `epistemic` filter array — each errors listing valid values | §5.2, §6 |
| `test_mcp_verify_tool` | verify registered, callable, correct results (I5a) | §5.5 |
| `test_mcp_task_tools` | task_current/task_complete registered, callable (I5b) | §5.5 |
| `test_mcp_resolve_invalidate` | action reachable over MCP | §5.5 |
| `test_mcp_breakdown_exposes_situation_multiplier` | `ScoreBreakdownOutput` serializes the new field (hand-enumerated struct — omission is the failure mode) | §7.1 |
| `test_mcp_list_include_invalidated` | `list` excludes by default, includes with flag | §14.14 |
| `test_cli_flags_parse` | clap parses all new flags/subcommands (mirror existing `Cli::try_parse_from` tests) | §5.3 |
| `test_cli_clear_validity_behavior` | `update --clear-validity` maps to all-empty-Validity ⇒ `valid_while: None` on disk (behavior, not just parse) | §5.3, §2.3 |
| `test_output_tags` | pretty: `[fact]` off-diagonal-only; `[invalidated <date>]` under include-invalidated. json: `epistemic` always present; validity/bi-temporal fields present-when-set | §5.4 |

### 1.8 Hooks (`engram-cli`, I6)

| Test | Asserts | Spec |
|------|---------|------|
Harness split: rendering/grouping/budget tests are inline `#[cfg(test)]`
units in `commands/hook.rs` (existing pattern); process-level assertions
(exit codes, stdout) live in the assert_cmd harness at
`crates/engram-cli/tests/cli/hook.rs`, reusing its `hook_cmd()` helper —
which pins empty `ENGRAMDB_MODEL_CACHE_DIR` + `ENGRAMDB_OFFLINE=1` so an
accidental model load fails fast. Names in that harness drop the `test_`
prefix per its local convention.

| Test | Asserts | Spec |
|------|---------|------|
| `test_hook_groups_by_class` | class-primary grouping, type tags per line, situation-dependent order; an invalidated memory in the store never appears in any hook output (§14.14) | §8.1 |
| `test_hook_decision_rendering` | "— because {premise}" when present; summary-only when absent (never invented) | §8.2 |
| `test_hook_decision_atomicity` | over-budget decision entry skipped whole, never mid-rationale truncation; total ≤ 2000; same atomicity asserted at the 1000-char prompt budget (where over-budget is the common case) | §8.4, §14.9 |
| `test_hook_task_gating` | task-scoped memory absent for non-matching session; present when task matches (I7 wiring); still retrievable by explicit query when suppressed; hint line shows the **exact** hidden count and only when the task tools are registered | §8.3, §16.4 |
| `test_hook_class_order_override` | `[hooks] class_order` overrides the per-situation defaults uniformly | §8.4, §12 |
| `test_hook_user_prompt_submit` | prompt → filter query results injected under prompt budget; situation inference table (each keyword class + no-match) | §8.5.1 |
| `test_hook_post_tool_use_watch_match` | edited path matching `watch_paths` ⇒ warning names memory; no match ⇒ empty output; watcher on an **invalidated** memory ⇒ no warning (§2.4 guard) | §8.5.2, §14.14 |
| `test_hook_session_end_flush` | I6: telemetry flushed. I7 wiring: mapping cleared; demotion only when configured | §8.5.3 |
| `test_hook_pre_compact_output` | emits the reminder text (or, if the additionalContext decision lands no-op: registered, exit 0, empty output) | §8.5.4 |
| `hook_*_malformed_stdin_exit_zero` (assert_cmd) | every new subcommand: empty/garbage stdin ⇒ exit 0, empty stdout | §14.15 |

### 1.9 Lifecycle (core, I7)

| Test | Asserts | Spec |
|------|---------|------|
| `test_task_mapping_roundtrip` | session→task write/read/clear; atomic file discipline | §11.1 |
| `test_task_complete_demotes` | task-generality decisions flip to exp 14d; explicit user decay untouched; project-generality only noticed | §11.2 |
| `test_demotion_replaces_curve` | demoted memory's score reflects new curve only (no stacking; §14.6) | §11.2 |
| `test_promotion_counts_distinct_sessions` | origin session excluded; threshold triggers suggestion; auto path only under flag; promotion clears origin_task, stamps verified_at, never retypes | §11.3 |
| `test_promotion_no_telemetry_noop` | memory with zero telemetry rows / store predating the telemetry extension ⇒ promotion pass is a clean no-op | §11.3, §14.11 |
| `test_consolidation_cluster_gate` | similarity + no-contradiction gates; Fact created with derived_from; sources demoted never deleted; demotion **replaces** any prior curve (invariant 6) | §11.4 |
| `test_lifecycle_ops_no_providers` | `task_complete`, promotion pass, consolidation pass, and `verify` all succeed (or cleanly skip with logged notice) with providers absent — pin determinism with `ENGRAMDB_MODEL_CACHE_DIR`=empty + `ENGRAMDB_OFFLINE=1` (the `stats_embeddings_status_…_model_missing` pattern), or build the engine with providers explicitly absent | §14.11 |

Consolidation gate logic is tested with **stub providers** (hand-crafted
vectors via the existing `StubEmbeddingProvider`/`MarkerEmbeddingProvider`
seams, synthetic `NliResult`s) — deterministic, no group membership needed.
If a real-model E2E is added, it must be **explicitly registered** in
`.config/nextest.toml`'s `ml-models` filter (membership is a hand-written
filter clause, not automatic) and accepts the known flaky-environment
caveats.

### 1.10 Teaching & packaging (I8)

| Test | Asserts | Spec |
|------|---------|------|
| `mcp_tool_suffixes_match_server_tools` (existing, extended) | exact-equality pin: `verify` added **in I5a's PR**, `task_current`/`task_complete` **in I5b's** — the suffix list moves with its tools, never later, or every intermediate increment's workspace gate fails | §14.15 |
| `test_plugin_setup_hook_lockstep` | plugin.json hook event set == setup.rs settings-fallback event set (parse the real plugin.json) | §16.3 |
| `test_engram_md_rewrite_idempotent` | old-content file replaced once; second run no-op; `@ENGRAM.md` ref never duplicated (extend existing tests) | §14.16 |

## 2. Integration & end-to-end validation

### 2.1 Scripted store walkthrough (manual or shell-scripted)

Steps 1–4 and 6–7a run after I5a+I6; steps 5, 7b, and 8 need I7+I5b — the
**full** walkthrough is the post-I7 checkpoint. In a scratch project
(`ENGRAMDB_DATA_DIR`/`ENGRAMDB_CONFIG_DIR` pointed at temp dirs, models
staged per CLAUDE.md sandbox notes; hooks invoked via manual stdin —
`echo '<event json>' | engramdb hook <event>` — since live wiring lands in
I8):

1. `create` a fact (convention), an observation (`--epistemic observation
   --invalidated-by Cargo.lock`), and a task decision (`--premise "because
   rate limits" --origin-task demo --generality task`).
2. `query --situation debugging` vs `--situation session_start` — observation
   ranks above/below accordingly. Spot-check `situation_multiplier` via an
   MCP `query` call (the CLI does not print breakdowns; MCP
   `ScoreBreakdownOutput` is the observability surface, spec §7.1).
3. Touch `Cargo.lock`; run the post-tool-use hook with a synthetic event —
   warning names the observation. `doctor` reports the same; `doctor --fix`
   flips it; `verify` restores Active and stamps `verified_at`.
4. `create` a superseding decision with `--supersedes <old>` — old memory
   invisible to default `query`/`list`, visible with `--include-invalidated`
   carrying `[invalidated]` tag and `superseded_by`. Then
   `update --clear-invalidated` the old memory and confirm it reappears
   (reopening, §2.4).
5. *(post-I7)* `task complete demo` — decision decay flips; hook injection
   stops showing it after decay bites (spot-check score via MCP).
6. `gc --dry-run` shows nothing before retention; with
   `invalidated_retention_days = 0`-override test config, confirm keep-forever.
7. (a) Session-start + user-prompt-submit hooks against the store: budgets
   respected, decisions atomic. (b) *(post-I5b)* hint line when task-scoped
   memories hidden.
8. *(post-I7)* `compress_candidates`/`compress_apply` on low-criticality
   memories — sources invalidated (not deleted), `superseded_by` = summary
   id, visible under `--include-invalidated`.

### 2.2 Migration validation (after I2, re-run after I8)

Copy a real pre-change store (or generate with a pinned old binary):
open with the new binary → version 0.3.0, `stats` unchanged counts,
`query` results unchanged for `situation: None`, vectors not re-embedded
(fingerprint untouched — `doctor` clean).

### 2.3 Hook latency budget (after I6)

On a store of ~500 memories, **release build, warm cache** (a debug binary
statically linking lancedb/datafusion can blow the budget on store open
alone — the tripwire is meaningless without the profile pinned):
`session-start`, `pre-tool-use`, `user-prompt-submit`, `post-tool-use`, and
`session-end` each complete < 150 ms without a daemon (hooks are
process-per-invocation, so whole-process wall time is the meaningful
number — not a criterion bench, which CI deliberately excludes). Measured
via a simple timing loop (hyperfine if available — it is not installed in
the web sandbox); per-event numbers recorded in the PR description. This is
a regression tripwire, not CI-enforced (CI hardware varies).

## 3. Behavioral-change acceptance (release gate)

The intended default behavior changes on upgrade — all **four**, matching
spec §13's numbered list:

1. Hook-driven queries now carry a situation (D4 tuned profiles). Validate:
   diff session-start injection on a populated store before/after — ordering
   changes are class-coherent; high-criticality exact-scope memories never
   disappear (§14.5 guarantees exactly that case — a borderline memory
   scoring 0.46–0.55 pre-change *can* legitimately drop below the 0.45
   threshold under a down-weighting profile).
2. Per-class challenge-penalty defaults (flat 0.10 → 0.15/0.20/0.05 when no
   scalar is configured). Validate: `test_challenged_default_penalty_shift_intended`
   (§1.4) asserts the exact intended deltas.
3. New `supersedes` writes close windows. Validate: §1.6 tests; confirm old
   stores' historical supersedes lists still resolve nothing retroactively
   (fixture with dangling supersedes unchanged on open); walkthrough step 4.
4. `compress_apply` invalidates instead of deletes. Validate: §1.6 test +
   walkthrough step 8.

Everything else must be provably inert: §1.4's golden-value tests (scoped
to unchallenged memories / Flat config per spec §14.4) are the enforcement.

## 4. CI

- All new tests run under the standard
  `cargo nextest run --workspace --all-features` gate — no new CI jobs.
- Any real-model test must be explicitly added to the hand-written
  `ml-models` filter in `.config/nextest.toml` (membership is not
  automatic).
- Fuzz targets run on the scheduled `fuzz.yml`, which runs a
  **hand-enumerated target matrix** (it does not enumerate `[[bin]]`s —
  that's local `cargo fuzz build`). The two extended targets are already in
  the matrix, so no workflow change; a future *new* target would need a
  matrix entry.
- `check-cross` (macOS/Windows) runs a model-free filtered subset
  (`daemon::transport`, `paths::tests`, `write_lock::tests`) — I7's
  task-mapping path helpers, if placed in `paths`, must stay model-free or
  they break cross-platform CI.
- Doc-only increments still run the full gate (repo policy).

## 5. Rollback validation

**Read-safety is structural, verified:** `deny_unknown_fields` appears
nowhere in the repo — the old `MinimalFrontmatter`/`HiddenMeta` serde parse
ignores the new keys by construction, and `parse_hidden_meta` degrades to
defaults on any YAML error rather than failing. No vendored-parser test
(the old parser depends on the private `helpers` module; a copy would
drift). Instead:

1. **Pre-release manual check with a pinned binary**: git worktree at the
   last pre-0.3.0 release tag → build → run `get`/`list`/`query` against a
   store written by the new binary. Scripted once, run before release.
2. **Write-back loss is the real downgrade hazard**: an old binary that
   rewrites a file (update, challenge, compress, access-stamp) silently
   drops `valid_while`/`invalidated_at`/`epistemic`/etc. — in particular,
   dropping `invalidated_at` **resurrects** an invalidated memory. The
   pinned-binary check should demonstrate this once, and the release notes
   must state it (spec §13 downgrade caveat): downgrade is read-safe, not
   write-safe.
