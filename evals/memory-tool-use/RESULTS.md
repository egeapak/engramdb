# Memory-tool-use eval: step-by-step results

Claude Code 2.1.286 with the `engram` plugin, run against a seeded fixture project.
41 cases (44 on the vague fixture) × 3 repeats per variant. Programmatic grading only (`grade.py`).
Each variant changes one thing; the change is listed in its `change.md`.
Raw rows, transcripts and `gaps.md` per variant are in `.claude/hillclimb/memory-tool-use/<variant>/`.
Regenerate the tables with `summarize.py`; re-grade with `regrade.py`.

**Grading rule (changed after step 3, all variants re-graded):** `consulted` passes when Claude called `query`/`get` before acting **or** a hook already injected the target memory's title and body. `query first` (the old rule) is reported but no longer part of `pass`. Steps 1–2 are unaffected: their hooks never showed bodies.

## Default fixture

| Variant | Model | Change | pass | consulted | query first | explicit create | implicit capture | revise | no false create | no spurious revise | store-read runs | get calls | $/case | input tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| baseline | Opus 5.5 | plugin only | 59% (73/123) | 24% (13/54) | 24% (13/54) | 67% (6/9) | 53% (8/15) | 50% (6/12) | 98% (85/87) | 100% (111/111) | 12 | 0 | 0.105 | 72k |
| v2 | Opus 5.5 | + ENGRAM.md (step 1) | 81% (100/123) | 67% (36/54) | 67% (36/54) | 100% (9/9) | 100% (15/15) | 67% (8/12) | 99% (86/87) | 100% (111/111) | 16 | 0 | 0.120 | 85k |
| v4 | Opus 5.5 | + pinned core tools (step 2) | 93% (115/123) | 94% (51/54) | 94% (51/54) | 100% (9/9) | 100% (15/15) | 75% (9/12) | 99% (86/87) | 99% (110/111) | 11 | 0 | 0.109 | 85k |
| v6 | Opus 5.5 | + ids and bodies in hooks (step 3) | 95% (117/123) | 100% (54/54) | 50% (27/54) | 100% (9/9) | 100% (15/15) | 83% (10/12) | 99% (86/87) | 97% (108/111) | 7 | 9 | 0.106 | 86k |
| v12 | Opus 5.5 | + routing to EngramDB (step 4) | 90% (111/123) | 98% (53/54) | 35% (19/54) | 100% (9/9) | 100% (15/15) | 75% (9/12) | 95% (83/87) | 96% (107/111) | 6 | 3 | 0.113 | 90k |
| v1 | Sonnet 5.5 | plugin only | 45% (55/122) | 2% (1/53) | 2% (1/53) | 0% (0/9) | 0% (0/15) | 50% (6/12) | 100% (86/86) | 100% (110/110) | 13 | 0 | 0.050 | 57k |
| v3 | Sonnet 5.5 | + ENGRAM.md (step 1) | 76% (94/123) | 50% (27/54) | 50% (27/54) | 89% (8/9) | 53% (8/15) | 92% (11/12) | 100% (87/87) | 100% (111/111) | 5 | 2 | 0.059 | 67k |
| v5 | Sonnet 5.5 | + pinned core tools (step 2) | 95% (117/123) | 94% (51/54) | 94% (51/54) | 100% (9/9) | 60% (9/15) | 75% (9/12) | 100% (87/87) | 100% (111/111) | 0 | 0 | 0.053 | 70k |
| v7 | Sonnet 5.5 | + ids and bodies in hooks (step 3) | 94% (116/123) | 94% (51/54) | 69% (37/54) | 100% (9/9) | 60% (9/15) | 67% (8/12) | 100% (87/87) | 100% (111/111) | 1 | 33 | 0.051 | 65k |
| v13 | Sonnet 5.5 | + routing to EngramDB (step 4) | 94% (116/123) | 94% (51/54) | 46% (25/54) | 100% (9/9) | 87% (13/15) | 67% (8/12) | 100% (87/87) | 100% (111/111) | 0 | 18 | 0.053 | 66k |

## Vague-title fixture (facts only in memory bodies)

| Variant | Model | Change | pass | consulted | query first | explicit create | implicit capture | revise | no false create | no spurious revise | store-read runs | get calls | $/case | input tokens |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| v8 | Opus 5.5 | step 2 binary | 89% (117/132) | 100% (63/63) | 100% (63/63) | 100% (9/9) | 100% (15/15) | 75% (9/12) | 96% (92/96) | 93% (112/120) | 10 | 0 | 0.116 | 97k |
| v10 | Opus 5.5 | step 3 binary | 88% (116/132) | 100% (63/63) | 52% (33/63) | 100% (9/9) | 100% (15/15) | 83% (10/12) | 94% (90/96) | 92% (111/120) | 12 | 34 | 0.111 | 92k |
| v9 | Sonnet 5.5 | step 2 binary | 98% (129/132) | 100% (63/63) | 100% (63/63) | 100% (9/9) | 60% (9/15) | 75% (9/12) | 100% (96/96) | 100% (120/120) | 0 | 0 | 0.056 | 75k |
| v11 | Sonnet 5.5 | step 3 binary | 98% (129/132) | 100% (63/63) | 83% (52/63) | 100% (9/9) | 60% (9/15) | 75% (9/12) | 100% (96/96) | 100% (120/120) | 0 | 65 | 0.053 | 70k |


`fact_used` is 100% (or close) everywhere, so it is left out of the tables.

## Notes per step

- **Step 1, ENGRAM.md** (`engramdb setup`, the README's recommended install). The largest single gain on both models. Sonnet goes from never saving to EngramDB (0/9 explicit) to 8/9.
- **Step 2, pinned core tools** (`[mcp].always_load`; `_meta["anthropic/alwaysLoad"]` on query, get, create, update, challenge). Sonnet's ToolSearch calls fall from 74 to 0. Input tokens rise by 0.4% (Opus) and 3.6% (Sonnet).
- **Step 3, ids and bodies in hooks** (and the `DetailLevel::Summary` bug fix). Pass is flat on both models and both fixtures. Behavior shifts: Claude answers from the injected body instead of querying (`query first` falls, `consulted` stays at or above step 2), uses the ids (`get` calls: Opus 0 → 9, Sonnet 0 → 33), and Opus reads the store files directly in fewer runs (11 → 7). The vague fixture did not separate the binaries: with pinned tools Claude already queried every time, and `query` returns the full body.
- **Step 4, routing to EngramDB** (instructions, `create` description, nudge, PostToolUse auto-memory note, combined ENGRAM.md edit). Sonnet's implicit capture rises 60% → 87% and its auto-memory writes fall to 0 (from 2). Opus pass falls 95% → 90%, but its new misses are mostly valid: it challenged the test memory's `docker compose` claim (the fixture has no compose file) and saved real environment facts (`make test-fast` cannot run in the sandbox: no pytest/requests, `src` not importable). Those are fixture flaws, not regressions; fix before hill-climbing.
- **Vague fixture caveat:** some of its memory bodies claim things the fixture does not back up (a docker compose file, per-environment flags). Opus correctly challenged them, which `no spurious revise` counts as a miss. Fix before hill-climbing.
- `revise` has n = 12 per variant; differences under about 25 points are within noise.
- "store-read runs" counts runs that read `.engramdb/` around the MCP tools; searches that *exclude* `.engramdb/` are not counted (the detector was fixed and all variants recomputed).
