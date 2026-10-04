# EngramDB memory-tool-use eval: final report

Claude Code 2.1.286 with the `engram` plugin, run against seeded fixture
projects. Both models, Opus 5.5 and Sonnet 5.5, 3 repeats per case.
Programmatic grading only (`grade.py`). Every number below can be recomputed
from the `results.jsonl` files under `.claude/hillclimb/`.

## Headline

| | Start | End |
|---|---|---|
| Original eval (47 cases, after step 7) | Opus 59%, Sonnet 45% (plugin only, step 0) | Opus 100%, Sonnet 99% |
| Hard eval, pass (45 cases × 3 reps = 135 runs) | Opus 125, Sonnet 121 (baseline) | Opus 129, Sonnet 131 (mean of v4 and v4b) |
| Hard eval, $ per case | Opus 0.237, Sonnet 0.113 (baseline) | Opus 0.231 (−2.4%), Sonnet 0.106 (−6.4%) |

- **Quality.** On the hard eval, the climb raised pass by 4 runs on Opus and 10 on Sonnet. It then cut cost on both models.
- **Test split.** It started at 96% on both models, so it can show a change of only about one case. The whole-set numbers above are therefore **directional**.
- **Cost.** The cost cut against the incumbent before the cost climb (v2) is larger: −5.5% on Opus and −10.2% on Sonnet. It reproduced on a fresh confirm run.
- **Recommendation.** Merge the product changes listed under "Changes in the codebase". Each one is tied to a measured behavior change.

## Phase 1: building the eval and fixing what it found (steps 1–7)

Original fixture: a Python billing project with 10 seeded memories. Steps 1–4
used 41 cases. Step 5 fixed the fixture and grew the set to 47 cases, so the
rows from step 5 onward are not comparable with the rows above them.

| Step | Change | Opus pass | Sonnet pass | Main effect |
|---|---|---|---|---|
| 0 | Plugin only | 59% | 45% | Sonnet never saved to EngramDB (0/9 explicit creates) |
| 1 | ENGRAM.md instructions (`engramdb setup`) | 81% | 76% | Largest single gain |
| 2 | Pin core tools (`alwaysLoad`) | 93% | 95% | Sonnet's ToolSearch calls 74 → 0 |
| 3 | Memory ids and bodies in hooks | 95% | 94% | Claude answers from injected bodies |
| 4 | Route saves to EngramDB, not auto-memory | 90% | 94% | Sonnet implicit capture 60% → 87% |
| 5 | Rank fallback; memory-file read hint (47 cases) | 95% | 91% | Real-query rank recall 52% → 97%; empty rank results 45% → 0% |
| 6+7 | Challenge on the user's word; file-hook fixes; master merged | 100% | 99% | Revise when the repo still matches the old memory: 3/9 → 9/9 |

The details are in `RESULTS.md`.

## Phase 2: hill climb on a harder eval

The original eval hit its ceiling, so a new case set was built:
`cases_hard.jsonl`, 45 cases in 7 categories, with 43 seed memories and
`fixture_hard/`.

- **Categories:** multi-memory tasks, facts buried past the hook preview, stale or superseded memories, distractors, multi-turn sessions, facts discovered mid-task, and hard negatives.
- **Split:** 22 train and 23 test, stratified by category. The analyzer subagent read only train transcripts.
- **Saving discovered facts (your rule):** a fact found during a task must be saved only when finding it was costly, meaning 2 or more failed tool calls in that run. Otherwise saving is optional and not graded.

| Round | Change | Opus whole / test | Sonnet whole / test | $ per case (Opus / Sonnet) | Verdict |
|---|---|---|---|---|---|
| 0 | baseline | 125 / 67 | 121 / 66 | 0.237 / 0.113 | — |
| 1 | `challenge` on any contradicting source, not only the user | 132 / 68 | 122 / 68 | 0.250 / 0.117 | kept |
| 2 | File hook picks memories by nearest enclosing scope | 130 / 67 | 133 / 68 | 0.245 / 0.118 | kept |
| 3 | Cost: each memory once per session, in full | 129 / 67 | 129 / 69 | 0.232 / 0.104 | rejected (Sonnet outside quality band) |
| 4 | Cost: v3 + a fact's premise shown as a check | 128 / 65 | 131 / 69 | 0.231 / 0.106 | **kept** |
| 5 | Cost: unpin `update`, shorter parameter text | 132 / 68 | 131 / 69 | 0.231 / 0.105 | rejected (no cost change), reverted |
| 4b | Confirm run of v4 (same code) | 130 / 66 | 131 / 69 | 0.231 / 0.106 | confirms v4 |

Whole is out of 135 runs; test is out of 69.

- **Noise floor.** One case is 3 runs, about 2.2 points of whole-set pass. The cost noise is about ±2.5–2.8% per variant pair.
- **Guardrails.** No-false-create and no-spurious-revise stayed within 2 runs of the baseline on every kept round.

### Why each kept change worked

- **Round 1.** Step 6 had narrowed `challenge` to "the user says a memory is outdated". Both models then found contradictions in repo files, such as a dated runbook or a dependency pin, explained them, and left the memory untouched. Widening the trigger fixed every revise target on Opus and `dc-runbook-export` on Sonnet. The same sentence lists what does not count as a contradiction, which kept spurious challenges at zero.
- **Round 2.** Sonnet acts on what the file hook injects. The hook took its 5 memories by score, so two kinds of memory took every slot from the edited directory's own conventions:
  - a memory about a sibling file, which has the same proximity as the directory itself;
  - a high-importance project-wide hazard.

  Picking by nearest enclosing scope lifted Sonnet from 122 to 133. `mm-refund-endpoint`, `st-flask-premise` and `mt-refund-after-drift` went to 3/3.
- **Round 4 (cost).**
  - **Where the cost was:** cache reads are about 80% of a case's cost, and each turn re-reads the whole context. So the lever is the number of turns. About 2 of 8 turns were memory calls, mostly `get`s of memories a hook had shown cut short. Half of all hook entries repeated a memory the session already held.
  - **v3:** shows each memory once, in full. `get`s fell from about 1.5 to 0.1 per case. But Sonnet then followed a memory whose premise had lapsed: the `get` it no longer made had been its moment to check the memory.
  - **v4:** puts the premise on the line as a check to make. That restored the check and kept the saving.

### What did not work, and why that matters

- **v5 (trimming cached text).** Removing about 2k tokens of tool schema from every turn changed cost by 0% (Opus) and −0.8% (Sonnet). Cached input is cheap at these prices, so text that triggers no extra action is nearly free. Further cost work should target turns and tool calls, not prompt length.
- **Raw round-1 numbers looked like an overfit** (train up, test down). The "spurious" challenges were mostly of memories the fixture contradicts on purpose, or that a newer seed supersedes. The grader was fixed to count only revisions of memories that are still correct. Every variant was re-graded, and the ranking did not change.

## Before and after (Sonnet)

**Stale premise (`st-flask-premise`).** The memory says to use
`@bp.route(..., methods=[...])` because production pins Flask 1.1. The repo
requires `flask>=3.0`.
- **Baseline:** the file hook never showed the memory. Sonnet used `@bp.get`, but left the stale memory in place.
- **v4:** the hook shows it with "holds only while production pins Flask 1.x: check that before following it". Sonnet grepped the repo and called
  `challenge` with: "pyproject.toml declares flask>=3.0, and src/api/invoices.py already uses @bp.get, so the Flask 1.x pin premise no longer holds."
- Traces: `memory-tool-use-hard/sonnet/baseline/traces/st-flask-premise_rep0.json` and `.../v4/traces/st-flask-premise_rep0.json`.

**Contradicting document (`dc-runbook-export`).**
- **Baseline:** Sonnet named the conflict between the runbook and the memory, recommended a dry run, and challenged nothing. In its debrief it said: "The memory has no date, so I couldn't tell whether it predates the 2026-09-24 bucket split in the runbook."
- **v1:** it called `challenge` with the runbook line as evidence.

**Cost (`mm-refund-endpoint`).**
- **v2:** 6 `get` calls, 18–19 turns, $0.16–0.17 per run.
- **v4:** 0 `get` calls, 9–11 turns, $0.12 per run.
- The test passed in all 6 runs.

## Changes in the codebase

Tags: **[REQUIRED]** fixes something broken; **[TUNE]** is a measured judgment call that you could decline.

1. **[REQUIRED]** Hooks rendered an empty body for `DetailLevel::Summary` (step 3).
2. **[REQUIRED]** A rank query returned nothing when no memory cleared the threshold. It now returns the best 3, marked `below_threshold`, with a hint. Keyword evidence no longer lowers a score (step 5).
3. **[REQUIRED]** The file hook injected nothing for default-importance memories. Scope, trust and situation multiplied their score below the threshold. A scope match now clears the threshold on the memory's own relevance; root-wide scopes are excluded (step 7).
4. **[REQUIRED]** The hook budget starved later entries. Every summary line is now placed first, then body previews follow in rank order (step 7).
5. **[TUNE]** The core tools are pinned with `_meta["anthropic/alwaysLoad"]`, configurable as `[mcp].always_load` (step 2).
6. **[TUNE]** Memory ids and bodies appear in hook output (step 3).
7. **[TUNE]** Routing text tells Claude to save project facts to EngramDB, not auto-memory. The PostToolUse hook adds a note when Claude writes auto-memory (step 4).
8. **[TUNE]** A hint appears when Claude reads a memory file directly (step 5).
9. **[TUNE]** `challenge` fires on any contradicting source, with the non-triggers listed (steps 6 and round 1).
10. **[TUNE]** The PreToolUse hook picks memories by nearest enclosing scope (round 2).
11. **[TUNE]** Each memory is injected once per session, in full.
    - The record lives at `.engramdb/state/hook_seen/<session>`. SessionStart and PreCompact clear it; SessionEnd deletes it.
    - `preview_chars` goes from 160 to 1000, and `prompt_context_budget` from 1500 to 3000.
    - A fact's premise is shown as a check (cost rounds, v4).
12. Merged from master: the stdio handshake is answered before slow startup work (the cold-start fix). It kept working with the pinned-tool code.

The eval itself lives in `evals/memory-tool-use/`:
- the runner;
- the grader, with fixes for consulted-via-hook, stale-by-design revises and costly discovery;
- both case sets and fixtures;
- `regrade.py` and `summarize.py`;
- the retrieval probe in `evals/retrieval-probe/`.

## Remaining failures

All are below the noise floor:
- **`st-retention` (Opus).** It saves real inconsistencies of the fixture: the docs refer to columns the migrations never add. This is a fixture flaw, not a product gap.
- **`mm-dunning-job` (Sonnet).** The needed memory is scoped to a module the edited file calls (`src/jobs/`), so no scope rule reaches it. This is a real but small gap.
- **`ds-fx-rounding`, `ds-notifier-flag`.** Grader artifacts: a contrastive comment, and a helper name that trips a forbidden-term check.
- **Costly-discovery cases.** They never triggered: both models read the script before running it, so no run hit 2 failed calls. The rule is in the grader, but the cases do not exercise it.

## What I would try next

- **Scope by dependency.** When a file is edited, also surface memories scoped to modules it imports or calls. This fixes `mm-dunning-job`.
- **Fewer turns, not shorter text.** The remaining memory turns are 0.5–0.8 per case, mostly `query` and `challenge`. Cost work should look there or at task turns, not at prompt length (see v5).
- **Daemon idle timeout.** During the eval, the embedding daemon did not exit after its idle timeout, so the runner stops it explicitly. A user running many sessions would accumulate daemons, about 400 MB each.
- **Fixture and grader polish.** Fix the `st-retention` schema gap. Loosen the two forbidden-term checks. Add discovery cases where running is the only way to find the cause.
- **A third, harder set.** Many more memories (100+), long sessions with compaction, and cross-module tasks. The hard set saturated after two rounds.

## Where to look

- `.claude/hillclimb/memory-tool-use-hard/{opus,sonnet}/report.html` — per-case scores per round, with links to every trace.
- `.claude/hillclimb/memory-tool-use-hard/narrative.md` — the running summary.
- `.claude/hillclimb/memory-tool-use-hard/{opus,sonnet}/vN/change.md` — each round's reason, prediction and result.
- `evals/memory-tool-use/RESULTS.md` — the original eval, steps 1–7.
- `evals/memory-tool-use/HARD_CASES.md` — the hard case set.
