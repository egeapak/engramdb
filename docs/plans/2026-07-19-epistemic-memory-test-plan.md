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
| `test_memory_new_epistemic_defaults` | `Memory::new` sets diagonal class, `valid_while: None`, bi-temporal fields None | §2.3 |
| `test_memory_update_apply_epistemic_and_validity` | update arms incl. all-empty-Validity-clears and invalidated_at set/clear (reopening) | §2.3, §2.4 |
| `test_is_active_invalidated` | `invalidated_at` past ⇒ not active; future ⇒ active; None ⇒ unchanged behavior | §2.4 |
| `test_situation_config_defaults_and_validate` | §7.1 table values; floor/profile ∈ [0,1] rejection; serde-default section absent ⇒ defaults | §7.1, §12 |
| `test_challenge_penalty_untagged_compat` | `challenge_penalty = 0.10` parses as Flat and `penalty_for(*) == 0.10`; table form parses PerClass; defaults 0.15/0.20/0.05; clamp on read | §7.2, §14.4 |
| `test_epistemic_config_defaults` | §12 `[epistemic]` defaults incl. `invalidated_retention_days = 180` | §12 |

### 1.2 File format (`engram-storage::memory_file`, I2)

| Test | Asserts | Spec |
|------|---------|------|
| `test_v2_roundtrip_off_diagonal_epistemic` | write→parse identity; `epistemic` key present only when off-diagonal | §3.1, §14.2 |
| `test_v2_roundtrip_validity_and_bitemporal` | `valid_while` + `valid_from`/`invalidated_at`/`superseded_by` roundtrip; empty Validity not emitted | §3.1 |
| `test_v2_parse_pre_epistemic_file` | checked-in fixture written by today's writer parses with §2.5 defaults, `valid_while: None` | §14.2 |
| `test_v1_parse_defaults_epistemic` | V1 path defaults from type | §3.2 |
| `test_v2_diagonal_writes_byte_identical` | a diagonal memory with no new fields produces byte-identical file output to the pre-change writer (golden fixture) | §3.1 |

Fuzz: `memory_file` / `memory_file_roundtrip` run ≥ 60s locally after I2
(`cargo +nightly fuzz run memory_file -- -max_total_time=60`); any crash
input committed as corpus seed (repo policy).

### 1.3 Index & migration (`engram-storage`, I2)

| Test | Asserts | Spec |
|------|---------|------|
| `test_index_row_carries_new_columns` | all 7 columns written/read for a fully-populated memory; `watch_paths` mirrors `valid_while.invalidated_by` | §4.1 |
| `test_migration_0_2_0_to_0_3_0` | fixture store stamped 0.2.0 (pre-epistemic files, with vectors) opens → version 0.3.0, columns materialized per defaults, `has_embedding`/vectors preserved | §4.1, §13 |
| `test_new_store_born_current` | fresh init stamps 0.3.0 directly | §4.1 |
| `test_default_query_excludes_invalidated` | index predicate excludes `invalidated_at` past; `include_invalidated` includes; expiry behavior untouched | §2.4, §14.14 |
| `test_invalidate_with_atomic` | `invalidate_with` closes window under write lock; concurrent challenge/edit survives (mirror existing `update_with` race tests) | §2.4, §14.14 |

### 1.4 Scoring (`src/scoring`, I3)

| Test | Asserts | Spec |
|------|---------|------|
| `test_situation_none_scores_unchanged` | `situation: None` ⇒ breakdown identical to pre-change values in all four weight modes (golden values from existing tests) | §14.4 |
| `test_situation_multiplier_floor_transform` | mult = `floor + (1-floor)*p` for each class × each default profile; breakdown records it | §7.1 |
| `test_situation_worst_case_above_threshold` | exact-scope, crit 0.8, human, unchallenged ⇒ ≥ 0.45 under every default profile value (mirror `test_trust_floor_prevents_extreme_compounding`) | §14.5 |
| `test_situation_nonfinite_config_neutral` | NaN/inf floor or profile ⇒ multiplier 1.0 | §7.1 |
| `test_challenge_penalty_per_class` | challenged fact/-obs/-decision lose exactly 0.15/0.20/0.05; Flat config loses flat value for all | §7.2 |
| `test_fact_freshness_anchor_verified_at` | Fact with decay curve + old created_at + recent verified_at scores as fresh; Observation/Decision anchor unchanged | §7.3 |
| `test_score_target_parity_new_fields` | `composite_score` ≡ `composite_score_target` incl. epistemic/verified_at (extend existing parity coverage) | §14.3 |

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
| `test_resolve_invalidate_action` | closes window, keeps file, optional superseded_by; reopening via update works | §5.5 |
| `test_verify_stamps_and_reopens` | `verified_at = now`; NeedsReview→Active only for doctor-originated findings (origin tag respected) | §10.4 |
| `test_gc_invalidated_retention` | dry-run lists, real run purges only past retention; `0` keeps forever | §2.4 |
| `test_compress_apply_invalidates_sources` | sources invalidated not deleted; `superseded_by` = summary id; `skipped_sources` for already-gone | §2.4.3 |
| `test_doctor_invalidated_path_check` | watched-glob mtime newer than anchor ⇒ finding; `--fix` flips NeedsReview; plain run mutates nothing (store hash-identical, §14.8) | §10.1 |
| `test_doctor_stale_observation_check` | age > threshold ⇒ finding; never flips even with `--fix` | §10.2 |
| `test_doctor_derived_from_cascade` | missing/challenged source ⇒ finding; one level only | §10.3 |
| `test_challenge_routing_matrix` | all 9 (new × existing) class pairs produce the §9.1 table outcome; decision-vs-decision sets NeedsReview never Challenged (§14.7) | §9.1 |
| `test_challenge_entrenchment_tiebreak` | same-class conflicts: trust class → recency → confidence → existing yields | §9.2 |
| `test_no_routing_path_deletes` | file count unchanged across every routing outcome | §14.7, §14.13 |

### 1.7 MCP + CLI (`engram-mcp`, `engram-cli`, I5)

| Test | Asserts | Spec |
|------|---------|------|
| `test_mcp_create_epistemic_params` | new CreateInput fields parse and reach ops; invalid class string → clear error | §5.2 |
| `test_mcp_query_situation_param` | situation string → enum; invalid → error listing valid values | §6.2 |
| `test_mcp_verify_task_tools` | verify/task_current/task_complete registered, callable, correct results | §5.5 |
| `test_mcp_resolve_invalidate` | action reachable over MCP | §5.5 |
| `test_cli_flags_parse` | clap parses all new flags/subcommands (mirror existing `Cli::try_parse_from` tests) | §5.3 |
| `test_output_tags` | `[fact]` off-diagonal-only; `[invalidated <date>]` under include-invalidated | §5.4 |

### 1.8 Hooks (`engram-cli`, I6)

| Test | Asserts | Spec |
|------|---------|------|
| `test_hook_groups_by_class` | class-primary grouping, type tags per line, situation-dependent order | §8.1 |
| `test_hook_decision_rendering` | "— because {premise}" when present; summary-only when absent (never invented) | §8.2 |
| `test_hook_decision_atomicity` | over-budget decision entry skipped whole, never mid-rationale truncation; total ≤ 2000 | §8.4, §14.9 |
| `test_hook_task_gating` | task-scoped memory absent for non-matching session; present when task matches; hint line appears with count | §8.3, §16.4 |
| `test_hook_user_prompt_submit` | prompt → filter query results injected under prompt budget; situation inference table (each keyword class + no-match) | §8.5.1 |
| `test_hook_post_tool_use_watch_match` | edited path matching `watch_paths` ⇒ warning names memory; no match ⇒ empty output | §8.5.2 |
| `test_hook_session_end_flush` | telemetry flushed; mapping cleared; demotion only when configured | §8.5.3 |
| `test_hook_malformed_stdin_exit_zero` | every new subcommand: empty/garbage stdin ⇒ exit 0, empty stdout | §14.15 |

### 1.9 Lifecycle (core, I7)

| Test | Asserts | Spec |
|------|---------|------|
| `test_task_mapping_roundtrip` | session→task write/read/clear; atomic file discipline | §11.1 |
| `test_task_complete_demotes` | task-generality decisions flip to exp 14d; explicit user decay untouched; project-generality only noticed | §11.2 |
| `test_demotion_replaces_curve` | demoted memory's score reflects new curve only (no stacking; §14.6) | §11.2 |
| `test_promotion_counts_distinct_sessions` | origin session excluded; threshold triggers suggestion; auto path only under flag; promotion clears origin_task, stamps verified_at, never retypes | §11.3 |
| `test_consolidation_cluster_gate` | similarity + no-contradiction gates; Fact created with derived_from; sources demoted never deleted; skips gracefully with no providers (§14.11) | §11.4 |

Consolidation tests that load models belong in the `ml-models` nextest group
(`max-threads = 1`); respect existing flaky-test caveats for
resource-constrained environments.

### 1.10 Teaching & packaging (I8)

| Test | Asserts | Spec |
|------|---------|------|
| `mcp_tool_suffixes_match_server_tools` (existing, extended) | passes with verify/task_current/task_complete | §14.15 |
| `test_plugin_setup_hook_lockstep` | plugin.json hook event set == setup.rs settings-fallback event set (parse the real plugin.json) | §16.3 |
| `test_engram_md_rewrite_idempotent` | old-content file replaced once; second run no-op; `@ENGRAM.md` ref never duplicated (extend existing tests) | §14.16 |

## 2. Integration & end-to-end validation

### 2.1 Scripted store walkthrough (manual or shell-scripted, after I6)

In a scratch project (`ENGRAMDB_DATA_DIR`/`ENGRAMDB_CONFIG_DIR` pointed at
temp dirs, models staged per CLAUDE.md sandbox notes):

1. `create` a fact (convention), an observation (`--epistemic observation
   --invalidated-by Cargo.lock`), and a task decision (`--premise "because
   rate limits" --origin-task demo --generality task`).
2. `query --situation debugging` vs `--situation session_start` — observation
   ranks above/below accordingly; breakdown shows `situation_multiplier`.
3. Touch `Cargo.lock`; run the post-tool-use hook with a synthetic event —
   warning names the observation. `doctor` reports the same; `doctor --fix`
   flips it; `verify` restores Active and stamps `verified_at`.
4. `create` a superseding decision with `--supersedes <old>` — old memory
   invisible to default `query`/`list`, visible with `--include-invalidated`
   carrying `[invalidated]` tag and `superseded_by`.
5. `task complete demo` — decision decay flips; hook injection stops showing
   it after decay bites (spot-check score).
6. `gc --dry-run` shows nothing before retention; with
   `invalidated_retention_days = 0`-override test config, confirm keep-forever.
7. Session-start + user-prompt-submit hooks against the store: budgets
   respected, decisions atomic, hint line when task-scoped memories hidden.

### 2.2 Migration validation (after I2, re-run after I8)

Copy a real pre-change store (or generate with a pinned old binary):
open with the new binary → version 0.3.0, `stats` unchanged counts,
`query` results unchanged for `situation: None`, vectors not re-embedded
(fingerprint untouched — `doctor` clean).

### 2.3 Hook latency budget (after I6)

On a store of ~500 memories: `session-start`, `pre-tool-use`,
`user-prompt-submit`, `post-tool-use` each complete < 150 ms without a
daemon (index-only paths; no model loads). Measured via `hyperfine` or a
simple timing harness; recorded in the PR description. This is a
regression tripwire, not CI-enforced (CI hardware varies).

## 3. Behavioral-change acceptance (release gate)

The **only** intended default behavior changes on upgrade (§13):

1. Hook-driven queries now carry a situation (D4 tuned profiles). Validate:
   diff session-start injection on a populated store before/after — ordering
   changes are class-coherent; nothing previously surfaced disappears
   entirely (floor guarantees).
2. `compress_apply` invalidates instead of deletes. Validate: §1.6 test +
   walkthrough step 4.
3. New `supersedes` writes close windows. Validate: §1.6 tests; confirm old
   stores' historical supersedes lists still resolve nothing retroactively
   (fixture with dangling supersedes unchanged on open).

Everything else must be provably inert: §1.4 golden-value tests are the
enforcement.

## 4. CI

- All new tests run under the standard
  `cargo nextest run --workspace --all-features` gate — no new CI jobs.
- Model-loading tests join the `ml-models` group in `.config/nextest.toml`.
- Fuzz targets run on the existing scheduled `fuzz.yml` (not per-PR); the
  two extended targets need no workflow change (they enumerate `[[bin]]`s).
- Doc-only increments still run the full gate (repo policy).

## 5. Rollback validation

Because files written by the new binary may carry new frontmatter/hidden
fields: verify the **old** binary (pre-0.3.0) can still parse a store
written by the new one — serde-unknown-field tolerance in the old parsers is
what makes downgrade safe. Test: fixture file with all new fields fed to the
old parser vendored in a test (or manually with a pinned binary before
release). If the old V2 parser rejects unknown hidden-meta fields, document
downgrade as reindex-required in release notes.
