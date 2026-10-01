# Plan: show memory IDs in hook context

Status: planned (not implemented). Source: eval run of 2026-10-01, `.claude/hillclimb/memory-tool-use/`.

## 1. Root cause

All three context hooks render through `format_class_entry` (`crates/engram-cli/src/commands/hook.rs:228-278`), and it never prints `m.id`.
- Every line ends in `(source: …)` from `source_marker` (`:46`).
- Callers:
  - `process_hook_input` (PreToolUse, `:513`)
  - `process_user_prompt_submit` (`:746`)
  - `build_session_start_context_reserving` (SessionStart, `:135`)
- Only PostToolUse shows an ID: `process_post_tool_use` (`:820-823`) uses `short_id`.

Effect in the eval:
- `ct-loguru` and `ct-just` make 0 memory calls, and they fail in all 3 reps on both models.
- `ct-rounding` passes only because a `Grep` for `ROUND_` happened to match the memory's filename.

The same function pushes a two-space preview line when `content` is empty. This is the bug in plan 02, and the tier-2 snapshot `hook_pre_tool_use_valid.snap` shows it.

## 2. How IDs resolve

`get`, `challenge` and `update` all go through `MemoryStore::get` (`crates/engram-storage/src/store.rs:719`), so all three accept a unique prefix.
- `find_memory_files` returns an error for an ambiguous prefix.
- `store.get` searches the shared dir first and returns on a hit there, without checking the personal dir.

## 3. Options

- **A. Full ID (recommended):** `- [decision] Tax uses ROUND_HALF_EVEN (id: 01a0f6c2-…-6a43d853d1d7; source: shared/human)`.
  - Never ambiguous, and it matches what `query` returns.
  - Costs about 42 chars per entry.
- **B. `short_id` (13 chars):** about 19 chars per entry. But for UUIDv7, those 13 chars are exactly the 48-bit millisecond timestamp, so memories created in the same millisecond collide (batch create, `compress_apply`, harvest).
  - Both in one dir: the call fails as "Ambiguous ID prefix", which `challenge` reports as `MemoryNotFound`.
  - One shared and one personal: `store.get` silently picks the shared one, so the write goes to the wrong memory.
- **C. Shortest unique prefix:** needs every ID in the store on each hook run. That adds latency to a hook that must fail open, to save about 20 chars.

**Placement:** inside the trailing parenthetical, so the summary stays first. Decision lines are atomic under budget, so the ID is never cut off.

**Budget:**
- `prompt_context_budget` (default 1000, `crates/engram-types/src/config.rs:1632`): 5 entries cost about 210 more chars.
- SessionStart (2000): 10 entries cost about 420 more chars.
- Count the omitted entries before raising any default.

**Trust:** shared IDs come from frontmatter in a cloned repo. `validate_id_shape` (`crates/engram-storage/src/memory_file/mod.rs:37`) only rejects path-hostile IDs. Print the ID only if it matches `^[A-Za-z0-9_-]{1,64}$`; otherwise print nothing.

## 4. Steps

1. Add `fn id_marker(m) -> Option<String>` next to `source_marker`, with that allowlist. Use it in all three branches of `format_class_entry`.
2. Skip an empty preview: `!preview.is_empty() && preview != m.summary`. Coordinate this with plan 02.
3. Optional: switch PostToolUse to the full ID, for the same collision reason.
4. Unit tests:
   - Update `test_format_class_entry_per_class` and `test_injected_context_marks_visibility_and_provenance`.
   - A hostile ID (with a harness tag) is not printed.
   - An empty content gives no preview line.
   - Re-check `test_budget_decision_atomicity_and_fact_compression`.
5. Regenerate the tier-2 snapshots `hook_pre_tool_use_valid` and `hook_session_start_min_criticality_*` (and `hook_post_tool_use_valid` if you do step 3). The `[UUID]` filter redacts the ID. Run `cargo insta test --accept --test-runner nextest` twice.
6. Change `ENGRAM_MD_CONTENT` in a later variant ("injected memories carry `id:`; pass it to `get`/`challenge`/`update`"), so each run measures one change.

## 5. Measuring it

1. Build: `cargo build --release`.
2. Quick check: `python3 evals/memory-tool-use/run.py --variant vN --model claude-opus-5-5 --cases ct-loguru,ct-just,ct-release,ct-rounding --reps 5`.
3. Then the full run.

Expected movement:
- `revise` on ct-* rises from 6/12. Success is ≥ 15/20 at 5 reps; n=12 is too small to separate.
- `no_spurious_revise` must not drop.
- `memory_calls` on ct-* rises from 0.
- Debriefs naming the missing ID fall from 15/41 toward 0.
- `.engramdb` greps fall.
- Turns and cost stay flat.

## 6. Risks and open questions

- **The ID may not be the real blocker.** ct-loguru and ct-just make 0 memory calls, and the tools are deferred behind ToolSearch (plan 04). If `revise` does not move, look there, or add a contradiction nudge.
- **Store routing:** hook queries fan in group and global stores. A memory from one of those fails `challenge`/`update` unless `project` is passed. Show the store when it is not the local project?
- **Budget:** raise `prompt_context_budget` to about 1200?
- **Legacy IDs:** omit the ones that fail the allowlist, or defang them with `harvest::defang_metadata`?
