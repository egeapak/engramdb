# Code-review remediation log

This document tracks the review findings raised by the multi-area code review,
the **scenario** that demonstrates each one, the **positive/negative tests**
written to prove it, and the **fix**. The workflow for every item is strict TDD:

1. Write tests that encode the buggy scenario (a "negative" test that should
   fail while the bug exists, plus "positive" tests that pin the
   already-correct neighbouring behaviour so the fix doesn't regress it).
2. Run the tests and **observe the failure** (red).
3. Fix the production code.
4. Re-run and **observe the pass** (green).

Where a finding turns out to be a non-bug on close inspection, that is recorded
explicitly with a test that proves the behaviour is already correct.

Legend: ✅ fixed & green · 🟡 verified non-bug (test added) · ⏳ pending

| # | Sev | Area | Finding | Status |
|---|-----|------|---------|--------|
| 1 | Critical | storage | `create` unchecked upsert orphans files across visibility | ✅ |
| 2 | High | cli | `get --global` + `--path`/`--raw` resolves wrong dir for Shared | ✅ |
| 3 | High | cli/ops | interactive/editor `add` persists unvalidated criticality/confidence | ✅ |
| 4 | High | core/types | negative `search.threshold` disables the relevance gate | ✅ |
| 5 | High | storage | `check_staleness` false-positives under checkout conflict | ✅ |
| 6 | High | storage | memory_file blockquote/visibility parsing corruption | ✅ |
| 7 | Medium | cli | JSON-stdout corruption across several commands | ✅ |
| 8 | Medium | cli | `stats --daemon` aborts on protocol mismatch instead of fallback | ✅ |
| 9 | Medium | mcp | `query` detail_level case-sensitivity drops `details` | ✅ |
| 10 | Medium | models | single-text `embed()` doesn't chunk → silent truncation | 🟡 safe-by-truncation (documented) |
| 11 | Medium | daemon | per-op counters increment on failed requests | ✅ |
| 12 | Medium | scope | mid-segment glob computes wrong proximity depth | 🟡 verified non-bug (depth is slash-counted) |
| 13 | Medium | scope | `matches()` vs `calculate_pattern_score` disagree | ✅ filter loosened to scorer |
| 14 | Medium | storage | nondeterministic tied-mtime duplicate resolution | ✅ |
| 15 | Medium | storage | `get_batch`/`batch_exists` silently skip corrupt files | ✅ logging + behaviour test |
| 16 | Medium | storage | telemetry `load_recent` full-table scan | ✅ (unlocked by lancedb 0.30) |
| 17 | Medium | daemon | metrics 2nd connection + `optimize` every persist | ✅ |
| 18 | Medium | cli | `--format`/`--json` not mutually exclusive | ✅ |
| 19 | Low | types | `embeddings.max_tokens` is dead config | ✅ |
| 20 | Low | types | `search.threshold > 1.0` warns but doesn't clamp | 🟡 non-bug (clamped at use); tests pin contract |
| 21 | Low | cli | `hook --min-criticality` accepts out-of-range/NaN | ✅ |
| 22 | Low | cli | `rollback --target-version` math masks bad input | ✅ |
| 23 | Low | cli | `conflicts_with` gaps (embeddings/index-only, tags) | ✅ |
| 24 | Low | models/cli | byte-vs-char length checks (keyword filter, id slicing) | ✅ |
| 25 | Low | models | Ollama batch lacks per-vector dimension check | ✅ |

---

## Details (batch 1: engram-storage + engram-types)

### #1 — `create` orphans files across visibility (Critical) ✅
- **Scenario:** create memory ID X as `Shared`, then `create` the same ID as
  `Personal`. The LanceDB row (keyed on ID) flips to Personal, but the old
  Shared `.md` was never removed → two files for one ID, disk≠index.
- **Tests** (`store.rs`): `create_same_id_different_visibility_does_not_orphan`
  (negative, red before fix), `create_basic_roundtrips_without_orphans`
  (positive).
- **Fix:** `create` now sweeps both visibility dirs for pre-existing files of the
  same ID and removes any that aren't the file just written (new-file-first
  ordering, mirroring `write_updated_locked`).

### #4 — negative `search.threshold` disables the gate (High) 🟡 (config part)
- **Scenario:** `[search] threshold = -0.1`. Consumer did `min(1.0)` → still
  negative → gate `if threshold > 0.0` skipped → every result returned.
- **Tests** (`config.rs`): `search_threshold_negative_is_rejected`,
  `search_threshold_nan_is_rejected` (negative, red), plus positive range tests.
- **Fix (this batch):** `EngramConfig::validate` rejects negative/NaN. The
  defensive engine-side `clamp(0.0, 1.0)` is done in batch 2 (core crate).

### #5 — `check_staleness` false-positive under checkout conflict (High) ✅
- **Scenario:** two clones of one git remote share a LanceDB index but have
  separate memory trees, so `lance_count > md_count` permanently → an
  un-actionable, never-clearing "run reindex" warning.
- **Tests** (`store.rs`): `staleness_message_suppressed_under_checkout_conflict`
  (negative, red), `staleness_message_reports_real_drift_without_conflict` (pos).
- **Fix:** extracted pure `staleness_message(md, lance, conflict)`; returns
  `None` when a conflict is present. `check_staleness` passes
  `self.checkout_conflict().await.is_some()`.

### #6 — memory_file blockquote/visibility parsing (High) ✅
- **Scenario A (content loss):** a `> ` line inside `## Content` was harvested
  into the unused `__blockquote__` key and dropped from the section.
- **Scenario B (visibility flip):** visibility was decided by a substring
  `contains("**Visibility:** personal")` over the whole Scope section.
- **Tests:** `helpers::blockquote_tests::blockquote_inside_section_stays_in_section`
  and `v2::visibility_tests::*` (negatives red before fix).
- **Fix:** blockquote harvesting gated to the preamble (`current_section.is_none`);
  visibility read from the `**Visibility:**` field *line* value.

### #14 — nondeterministic tied-mtime duplicate resolution (Medium) ✅
- **Scenario:** two duplicate files for one ID with identical mtimes (coarse FS
  granularity); `>=` + directory iteration order decided the winner
  nondeterministically, risking resurrection of stale content.
- **Test:** `prefers_newer_breaks_mtime_ties_deterministically` (negative, red).
- **Fix:** extracted pure `prefers_newer`; on an mtime tie the lexicographically
  greater path wins. Used by both `newest_by_mtime` and `reindex_dir`.

### #15 — `get_batch` silently skips corrupt files (Medium) ✅
- **Scenario:** an indexed memory whose file is unreadable/unparseable vanished
  from batch results with no log.
- **Test:** `get_batch_skips_corrupt_file_returns_valid` (pins skip-and-continue;
  behaviour is intentionally preserved).
- **Fix:** added `tracing::warn!` on read/parse failure (matching `reindex_dir`).
  This is observability, not a behaviour change, so the test documents the
  contract rather than flipping red→green.

### #20 — `search.threshold > 1.0` warns but doesn't clamp (Low) 🟡 non-bug
- **Finding re-examined:** the consumer (`engine.rs`) already applies `min(1.0)`,
  so a legacy >1.0 value behaves exactly as 1.0. The validate message is
  therefore accurate. No production change beyond keeping >1.0 *tolerated*
  (not a hard error) for back-compat; pinned by
  `search_threshold_above_one_is_tolerated_for_backcompat`.

## Details (batch 2: core engramdb)

### #4 — engine clamp (High) ✅
- **Test:** `retrieval::engine::tests::filter_threshold_is_bounded` (negative for
  `-0.5`→0 and `NaN`→0, positive for in-range/`>1`).
- **Fix:** `filter_threshold()` clamps to `[0,1]` and maps NaN→0; the Filter-mode
  gate uses it instead of `min(1.0)`. Combined with the batch-1 config rejection,
  the relevance gate can no longer be silently disabled.

### #11 — daemon counters on failure (Medium) ✅
- **Scenario:** an `Embed`/`Classify`/… that hits an unavailable model or a
  failing inference still bumped the persisted per-op counter, inflating
  `stats --daemon`.
- **Test:** `daemon::tests::failed_request_does_not_increment_counter` — forces
  embedding unavailable (empty model cache + offline), sends `Embed` (Error),
  asserts `Status.requests_embed == 0` (red before fix).
- **Fix:** counters are incremented only inside the success arm of each op.

### #12 — mid-segment glob depth (Medium) 🟡 non-bug
- **Re-examined:** proximity depth is computed by counting path separators
  (`directory_depth_from_parent`), which is unaffected by where the partial
  leading segment before the wildcard is cut — so the numeric score is already
  correct. The reviewer's specific example (`src/ap*` vs `src/api/handlers.rs`)
  is additionally a non-match (`*` does not cross `/`), short-circuited at the
  matcher. Pinned by `glob_midsegment_depth_is_correct` (passes before & after).

### #13 — filter vs scorer same-directory disagreement (Medium) ✅
- **Decision (user):** loosen the filter to match the scorer.
- **Scenario:** scope `src/api/a.rs`, query path `src/api/b.rs`. The proximity
  scorer rewards same-directory siblings (0.82, pinned by
  `test_proximity_same_directory_non_glob`), but `matches()` excluded them, so
  such memories never surfaced in Filter mode.
- **Test:** `matches_agrees_with_scorer_for_same_directory_sibling` (red before
  fix). `test_matches_exact` updated to the new (consistent) semantics.
- **Fix:** `matches()` now also returns true for `is_same_directory`. Full core
  suite (480 tests) still green — no filtering regressions elsewhere.

## Details (batch 3: engram-models)

### #10 — single-text embed() truncation (Medium) 🟡 documented/accepted
- Query/probe embedding truncation at the model token limit is standard, and
  fastembed truncates oversized chunks *safely* (no crash/corruption). A
  tokenizer-accurate chunker is a larger redesign and a char-budget cap
  conflicts with the established word-budget contract (existing tests). Pinned a
  safety test (`single_whitespace_free_blob_is_one_chunk_not_a_panic`) and
  documented the residual in `chunking.rs`.

### #24 — keyword single-letter filter byte-vs-char (Low) ✅ (models part)
- **Test:** `extract_keywords_drops_single_multibyte_letter` (red before fix).
- **Fix:** filter single-letter words by `chars().count() <= 1` instead of byte
  `len()`, so a single multibyte glyph ("é") is dropped like any other single
  letter. (Trade-off noted: this also drops lone CJK glyphs, acceptable for this
  English-RAKE-oriented extractor.) The CLI id-slicing half of #24 is batch 5.

### #25 — Ollama per-vector dimension check (Low) ✅
- **Test:** `validate_embedding_response_checks_dimensions` (wrong-width vector
  red before fix).
- **Fix:** extracted `validate_embedding_response()`; it now rejects any vector
  whose width ≠ the provider's declared `dimensions`, with a clear error,
  instead of feeding wrong-width vectors toward LanceDB.

## Details (batch 4: engram-mcp)

### #9 — query detail_level case-sensitivity drops details (Medium) ✅
- **Scenario:** `detail_level: "Full"` (any non-lowercase casing) parses
  correctly (engine keeps details) but the output gate recomputed inclusion via
  a case-sensitive `== "full"`, so details were silently stripped from the
  response.
- **Test:** `retrieve_detail_level_full_is_case_insensitive` — creates a memory
  with details, queries (filter + keyword signal) with `"Full"`, asserts details
  are returned (red before fix). Note: memory fields are flattened into each
  `memories[i]` object in the response JSON.
- **Fix:** derive `include_details` from the already-parsed `DetailLevel` enum
  (`matches!(detail_level, DetailLevel::Full)`), removing the duplicate
  case-sensitive string compare.

## Details (batch 5: engram-cli — clap/validation/path)

- **#2 (High)** `get --global --path/--raw`: resolve Shared memories from
  `store.project_dir` (the actual store, global or local) instead of the local
  `dir` param. Integration test `get_global_path_points_to_existing_file`
  asserts the printed global path exists (red before fix).
- **#18 (Medium)** `--format` now `conflicts_with = "json"`; clap rejects the
  combination instead of silently ignoring `--format`. Test `json_and_format_conflict`.
- **#21 (Low)** `hook session-start` sanitizes `--min-criticality` (clamp to
  [0,1], NaN→0.6) so a bad value can't filter out everything. Test
  `sanitize_min_criticality_bounds_and_handles_nan`.
- **#22 (Low)** `rollback --target-version` resolved via `resolve_rollback_target`,
  which rejects 0 and unsupported versions instead of silently writing v1. Test
  `resolve_rollback_target_validates`.
- **#23 (Low)** clap conflicts: `reindex --embeddings-only`/`--index-only` and
  `update --tags` vs `--tags-add`/`--tags-remove`. Tests
  `reindex_embeddings_only_and_index_only_conflict`,
  `update_tags_replace_conflicts_with_add_remove`.
- **#24 (Low)** `review` id display now truncates by characters
  (`chars().take(8)`), matching the char-safe `short_id` helper; keyword half
  was fixed in batch 3.

(Red runs for #18/#21/#23 demonstrated by temporarily reverting the guards;
all green after restore. Per-crate `-p` builds were avoided in favour of
`--workspace` runs because differing feature unification per `-p` target
forced full rebuilds of the lance/datafusion stack and exhausted the sandbox
disk.)

## Details (batch 6: core — #3, #19; perf items #16/#17)

### #3 — unvalidated scores on interactive/editor add (High) ✅
- **Fix:** `ops::create_memory` now calls `validate_score` on criticality and
  confidence, so EVERY front-end (direct flags, interactive, editor, MCP) is
  covered — not just the CLI direct path.
- **Test:** `ops::create::tests::create_memory_rejects_out_of_range_scores`
  (criticality>1, confidence<0, NaN all rejected; valid succeeds). Red verified
  by temporarily removing the guards.

### #19 — `embeddings.max_tokens` dead config (Low) ✅
- **Fix:** chunking now uses `effective_chunk_tokens(config_max, provider_max)`
  = `config.min(provider).max(1)`, so the config field can request smaller
  chunks (and never exceed the model's real limit). Threaded through
  `embed_memory_with`.
- **Test:** `effective_chunk_tokens_respects_config_capped_at_model`.

### #16 / #17 — perf optimizations (Medium) 🟡 documented, behaviour-correct
These are *performance* observations, not correctness bugs — current behaviour
is correct, so there is no failing test to turn green:
- **#16** `telemetry::load_recent` does a full scan + in-memory sort instead of
  an `ORDER BY ts DESC LIMIT cap` pushdown. Bounded by the 90-day retention and
  `STARTUP_REPLAY_CAP`.
- **#17** `daemon::metrics::persist_at` opens a second LanceDB connection for
  pruning and runs `optimize(All)` on every ~300 s persist (table is tiny).
Both are intentionally deferred: rewriting the LanceDB query/connection/optimize
patterns carries regression risk disproportionate to the gain (the reviewer
rated both low-blast-radius), and a behaviour-identical change cannot be guarded
by a red→green test in this sandbox. Tracked here for a future perf pass.

## Details (batch 7: engram-cli — JSON output #7, daemon fallback #8)

### #7 — JSON-stdout corruption (Medium) ✅ (full fix)
- **Foundation:** added `OutputFormatter::is_json()` so handlers can suppress or
  restructure human-oriented output under `--json`/non-TTY.
- **Single-JSON-object commands** (their output is parsed by scripts): `stats`,
  `stats --daemon`, `daemon status`, `gc`, `compress` now emit exactly one JSON
  value in JSON mode (previously formatter JSON + raw `println!` lines).
- **Progress chatter gated:** `reindex` progress lines suppressed in JSON mode.
- **Interactive commands** (`review`, `projects prune`) prompt for input, so JSON
  piping does not apply; left as human-only flows.
- **Tests:** `stats_json_is_valid_single_document`,
  `stats_daemon_json_is_valid_when_not_running`,
  `gc_json_dry_run_is_valid_single_document` parse stdout as a single JSON value
  (strict — trailing raw text fails). Red verified by disabling the gc JSON
  branch.

### #8 — `stats --daemon` aborts on protocol mismatch (Medium) ✅
- **Fix:** `run_daemon_stats` resolves the live status via
  `query_status(...).await.ok().flatten()`, collapsing both `Err(_)` (e.g. a
  protocol-version mismatch with an older daemon) and `Ok(None)` to "no live
  status" and falling back to the persisted snapshot — honouring the daemon
  graceful-fallback contract instead of exiting non-zero.
- **Test:** `stats_daemon_json_is_valid_when_not_running` exercises the fallback
  path end-to-end.

## Details (batch 8: deferred perf items, after merging master)

### #17 — daemon metrics: 2nd connection + optimize-every-persist (Medium) ✅
- **Fix:** `persist_at` now opens the LanceDB table **once** and reuses the handle
  for both the append and the prune (extracted `append_row(&table, …)`), instead
  of opening a second connection in the prune path. `prune_snapshots(&table)`
  first `count_rows(stale_predicate)` and returns early when nothing has aged
  out, so a steady-state daemon no longer issues an empty `delete` + full-table
  `optimize` on every ~300 s persist. `persist_row_at` is now `#[cfg(test)]`
  (only tests seed explicit-timestamp rows; production uses `append_row`).
- **Validation:** behaviour-preserving — existing `metrics_persist_then_load_latest`
  and `metrics_persist_prunes_old_snapshots` still pass (prune still removes aged
  rows; recent rows and cumulative seeding survive).

### #16 — telemetry `load_recent` full-table scan (Medium) 🟡 not actionable
- **Investigated:** the reviewer suggested pushing `ORDER BY ts DESC LIMIT cap`
  into LanceDB. Inspecting `lancedb` 0.26's `Query` API shows it exposes
  `limit`, `offset`, and `only_if` (filter) but **no ordering / sort** — exactly
  why `load_recent`'s own doc-comment reads every row then sorts in memory: a
  plain `limit(N)` returns rows in storage order with no recency guarantee.
- **Why not the offset-from-end trick:** `count_rows()` + `offset(count-cap)`
  would only return the most-recent rows if storage order matched insertion/ts
  order, which a background `optimize`/compaction can break — silently returning
  non-recent events. That trades a bounded perf cost for a correctness hazard.
- **Conclusion:** the in-memory sort is required for correctness with this
  LanceDB version, and the scan is bounded by the 90-day retention + prune. Left
  as-is; revisit if/when LanceDB exposes an ordered scan.

## Details (batch 9: after merging master a second time — deps #53 + doctor #54)

### #16 — telemetry `load_recent` full scan (Medium) ✅ now implemented
- **Previously** documented as not-actionable because lancedb 0.26's `Query`
  had no ordering. **The #53 dependency upgrade (lancedb 0.26 → 0.30) added
  `order_by(Vec<ColumnOrdering>)`**, so the pushdown is now available.
- **Fix:** `load_recent` now issues `ORDER BY ts DESC LIMIT cap` via
  `.order_by(Some(vec![ColumnOrdering::desc_nulls_last("ts")])).limit(cap)` and
  reverses the newest-first result into chronological order — instead of
  scanning the whole table and sorting in memory.
- **Test:** `load_recent_returns_newest_cap_in_chronological_order` (writes 5
  events, `cap = 3`, asserts the three *newest* are returned oldest-first).
  Existing ordering/prune tests remain green (behaviour-preserving).

### #54 doctor — added `validate_models` failure-path coverage
- The new `validate_models` diagnostic was only exercised by one happy-path CLI
  integration test; added `test_validate_models_reports_all_sections_when_unavailable`
  (forces the embedding model unavailable) to cover the failure + skip arms.

### #53 deps — no new coverage required
- Mechanical upgrade (rmcp 0.15→1.7 MCP-SDK rework, lancedb 0.26→0.30, arrow
  57→58, ONNX Runtime 1.24). No new logic; the reworked `server.rs` holds at
  ~89% line coverage and the whole suite (1546 passing) validates the adaptation.
