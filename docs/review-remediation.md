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
| 2 | High | cli | `get --global` + `--path`/`--raw` resolves wrong dir for Shared | ⏳ |
| 3 | High | cli/ops | interactive/editor `add` persists unvalidated criticality/confidence | ⏳ |
| 4 | High | core/types | negative `search.threshold` disables the relevance gate | 🟡 config validated; engine clamp pending |
| 5 | High | storage | `check_staleness` false-positives under checkout conflict | ✅ |
| 6 | High | storage | memory_file blockquote/visibility parsing corruption | ✅ |
| 7 | Medium | cli | JSON-stdout corruption across several commands | ⏳ |
| 8 | Medium | cli | `stats --daemon` aborts on protocol mismatch instead of fallback | ⏳ |
| 9 | Medium | mcp | `query` detail_level case-sensitivity drops `details` | ⏳ |
| 10 | Medium | models | single-text `embed()` doesn't chunk → silent truncation | ⏳ |
| 11 | Medium | daemon | per-op counters increment on failed requests | ⏳ |
| 12 | Medium | scope | mid-segment glob computes wrong proximity depth | ⏳ |
| 13 | Medium | scope | `matches()` vs `calculate_pattern_score` disagree | ⏳ |
| 14 | Medium | storage | nondeterministic tied-mtime duplicate resolution | ✅ |
| 15 | Medium | storage | `get_batch`/`batch_exists` silently skip corrupt files | ✅ logging + behaviour test |
| 16 | Medium | storage | telemetry `load_recent` full-table scan | ⏳ |
| 17 | Medium | daemon | metrics 2nd connection + `optimize` every persist | ⏳ |
| 18 | Medium | cli | `--format`/`--json` not mutually exclusive | ⏳ |
| 19 | Low | types | `embeddings.max_tokens` is dead config | ⏳ |
| 20 | Low | types | `search.threshold > 1.0` warns but doesn't clamp | 🟡 non-bug (clamped at use); tests pin contract |
| 21 | Low | cli | `hook --min-criticality` accepts out-of-range/NaN | ⏳ |
| 22 | Low | cli | `rollback --target-version` math masks bad input | ⏳ |
| 23 | Low | cli | `conflicts_with` gaps (embeddings/index-only, tags) | ⏳ |
| 24 | Low | models/cli | byte-vs-char length checks (keyword filter, id slicing) | ⏳ |
| 25 | Low | models | Ollama batch lacks per-vector dimension check | ⏳ |

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
