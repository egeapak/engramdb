# Memory-tool-use eval: step-by-step results

Claude Code 2.1.286 with the `engram` plugin, run against a seeded fixture project.
41 cases × 3 repeats per variant. Programmatic grading only (`grade.py`).
Each variant changes one thing; the change is listed in its `change.md`.
Raw rows, transcripts and `gaps.md` per variant are in `.claude/hillclimb/memory-tool-use/<variant>/`.

## Default fixture

| Variant | Model | Change | pass | query before act | explicit create | implicit capture | revise | no false create | $/case | input tokens |
|---|---|---|---|---|---|---|---|---|---|---|
| baseline | Opus 5.5 | plugin only | 59% (73/123) | 24% (13/54) | 67% (6/9) | 53% (8/15) | 50% (6/12) | 98% (85/87) | 0.105 | 72k |
| v2 | Opus 5.5 | + ENGRAM.md (step 1) | 81% (100/123) | 67% (36/54) | 100% (9/9) | 100% (15/15) | 67% (8/12) | 99% (86/87) | 0.120 | 85k |
| v4 | Opus 5.5 | + pinned core tools (step 2) | 93% (115/123) | 94% (51/54) | 100% (9/9) | 100% (15/15) | 75% (9/12) | 99% (86/87) | 0.109 | 85k |
| v1 | Sonnet 5.5 | plugin only | 45% (55/122) | 2% (1/53) | 0% (0/9) | 0% (0/15) | 50% (6/12) | 100% (86/86) | 0.050 | 57k |
| v3 | Sonnet 5.5 | + ENGRAM.md (step 1) | 76% (94/123) | 50% (27/54) | 89% (8/9) | 53% (8/15) | 92% (11/12) | 100% (87/87) | 0.059 | 67k |
| v5 | Sonnet 5.5 | + pinned core tools (step 2) | 95% (117/123) | 94% (51/54) | 100% (9/9) | 60% (9/15) | 75% (9/12) | 100% (87/87) | 0.053 | 70k |

`fact_used` is 100% in every variant: the fixture titles contain the answer, so the hooks alone supply it. The vague-title fixture (step 3) is built to remove that ceiling.

## Notes per step

- **Step 1, ENGRAM.md** (`engramdb setup`, the README's recommended install). The largest single gain on both models. Sonnet goes from never saving to EngramDB (0/9 explicit) to 8/9.
- **Step 2, pinned core tools** (`[mcp].always_load`; `_meta["anthropic/alwaysLoad"]` on query, get, create, update, challenge). Sonnet's ToolSearch calls fall from 74 to 0 and its direct reads of `.engramdb/` from 6 runs to 0. Input tokens rise by 0.4% (Opus) and 3.6% (Sonnet). `revise` moves within noise on both models (n = 12).
- `v2` was graded before the `tool_search` and `wrote_auto_memory` signals existed, so those fields are absent there, not zero.
