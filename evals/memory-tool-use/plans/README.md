# Memory-tool-use improvement plans

These plans come from the eval in `evals/memory-tool-use/`. Results are in `.claude/hillclimb/memory-tool-use/`: Opus 5.5 is `baseline`, Sonnet 5.5 is `v1`. Each plan was written by a planning agent and spot-checked against the code and the raw transcripts.

| # | Plan | Root cause | Main metric to move |
|---|---|---|---|
| 01 | [Memory IDs in hook output](01-hook-memory-ids.md) | `format_class_entry` never prints `m.id` | `revise` (6/12 on both models) |
| 02 | [Memory bodies in hook output](02-hook-memory-body.md) | **Bug:** hooks request `DetailLevel::Summary`, so the content is cleared and an empty preview line is printed | direct reads of `.engramdb/` (21 Opus / 16 Sonnet runs) |
| 03 | [Auto-memory competition](03-auto-memory-competition.md) | The system prompt says "save it immediately"; auto-memory needs only `Write`, while EngramDB tools are deferred | `explicit_create` (Sonnet 0/9) |
| 04 | [ToolSearch deferral](04-toolsearch-deferral.md) | MCP tools are deferred; Sonnet rarely takes the extra load step | `query_before_act` (Sonnet 1/53) |

The cold-start MCP bug (`engramdb serve` answers `initialize` after about 3 s, and Claude Code's discovery probe times out) is handled in a separate session.

## Overlaps to settle before implementing

- 01 and 02 both change `format_class_entry`. Implement them together: the ID goes on the summary line, and the preview's truncation marker points to `get <id>`.
- 03 (option B) and 04 (option A) are the same change: per-tool `_meta {"anthropic/alwaysLoad": true}`. Use 04's set (`query`, `get`, `create`, `update`, `challenge`) and its `[mcp] always_load` config key.
- 01, 03 and 04 all edit `ENGRAM_MD_CONTENT`. Make one combined edit, measured as its own variant.

## Suggested order (one measurable change per eval variant)

1. **Eval baseline for the recommended install:** rerun with `--engram-md` (the README tells users to run `engramdb setup`).
2. **Pin the core tools** (04-A / 03-B). Expected to move Sonnet the most.
3. **Hook IDs + bodies** (01 + 02, with the empty-preview bug fix). Add the vague-title fixture from plan 02.
4. **Routing text and the auto-memory write hook** (03-A, 03-C), plus the combined `ENGRAM.md` edit.

Grader additions needed along the way: `wrote_auto_memory` / `both_stores` (plan 03), the `tool_search` signal (plan 04), and the vague-title fixture and cases (plan 02).
