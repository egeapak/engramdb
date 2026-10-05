# EngramDB memory-tool-use eval: final report

Claude Code 2.1.286 with the `engram` plugin, run against seeded fixture
projects. Both models, Opus 5.5 and Sonnet 5.5, 3 repeats per case.
Programmatic grading only (`grade.py`). Every number below can be recomputed
from the `results.jsonl` files under `.claude/hillclimb/`.

## Headline

| | Start | End |
|---|---|---|
| Original eval (47 cases, after step 7) | Opus 59%, Sonnet 45% (plugin only, step 0) | Opus 100%, Sonnet 99% |
| Hard eval, pass (45 cases × 3 reps = 135 runs) | Opus 125, Sonnet 121 (baseline) | Opus 129, Sonnet 132 (mean of v4 and v4b) |
| Hard eval, $ per case | Opus 0.237, Sonnet 0.113 (baseline) | Opus 0.231 (−2.4%), Sonnet 0.106 (−6.4%) |
| Saving a costly discovery (after the climb, 6 cases × 10) | Opus 4/6, Sonnet 0/14 (v4d) | Opus 7/7, Sonnet 14/17 (v6, with ENGRAM.md) |
| Same, plugin-only install (no ENGRAM.md) | Opus 5/5, Sonnet 1/12 (v7p) | Opus 6/6, Sonnet 20/21 (v8p) |

- **Quality.** On the hard eval, the climb raised pass by 4 runs on Opus and 11 on Sonnet. It then cut cost on both models.
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
- **Saving discovered facts (your rule):** a fact found during a task must be saved only when finding it was costly. Otherwise saving is optional and not graded. The rounds below used "2 or more failed tool calls in the run" as the test for costly; see "After the climb" for why that changed.

| Round | Change | Opus whole / test | Sonnet whole / test | $ per case (Opus / Sonnet) | Verdict |
|---|---|---|---|---|---|
| 0 | baseline | 125 / 67 | 121 / 66 | 0.237 / 0.113 | — |
| 1 | `challenge` on any contradicting source, not only the user | 134 / 68 | 123 / 68 | 0.250 / 0.117 | kept |
| 2 | File hook picks memories by nearest enclosing scope | 130 / 67 | 133 / 68 | 0.245 / 0.118 | kept |
| 3 | Cost: each memory once per session, in full | 129 / 67 | 129 / 69 | 0.232 / 0.104 | rejected (Sonnet outside quality band) |
| 4 | Cost: v3 + a fact's premise shown as a check | 128 / 65 | 131 / 69 | 0.231 / 0.106 | **kept** |
| 5 | Cost: unpin `update`, shorter parameter text | 132 / 68 | 131 / 69 | 0.231 / 0.105 | rejected (no cost change), reverted |
| 4b | Confirm run of v4 (same code) | 130 / 66 | 133 / 69 | 0.231 / 0.106 | confirms v4 |
| 6 | Capture: ENGRAM.md line to save a discovery that cost effort | 130 / 67 | 133 / 69 | 0.235 / 0.111 | **kept** (see "Capture round") |
| 8 | Hook: ask once to save the cause after a failed Bash command succeeds | pilot 37/39 | 134 / 69 | — / 0.107 | **kept** (see "Retried-command prompt") |

Whole is out of 135 runs; test is out of 69. Re-graded after the climb with the fixed forbidden-term check (see "After the climb"): 5 train runs flipped to pass (Opus v1 +2, Sonnet v1 +1, Sonnet v4b +2). No verdict changes.

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
13. **[TUNE]** ENGRAM.md tells Claude to save a discovery that cost effort, without offering first (capture round v6). The MCP server instructions and the `create` description carry the same wording (v7p). That second part has no measured benefit for Sonnet; it is kept for consistency.
14. **[TUNE]** PostToolUse and PostToolUseFailure on Bash: when a command that failed earlier in the session succeeds, the hook asks once to save the cause if it is not documented (v8). State is in `.engramdb/state/bash_retry/<session>`; `setup` widens an existing PostToolUse matcher.
15. Merged from master: `[daemon]` moved to a global config, and the daemon got one private folder (#132).

The eval itself lives in `evals/memory-tool-use/`:
- the runner;
- the grader, with fixes for consulted-via-hook, stale-by-design revises and costly discovery;
- both case sets and fixtures;
- `regrade.py` and `summarize.py`;
- the retrieval probe in `evals/retrieval-probe/`.

## After the climb: fixture and grader fixes

A review of the remaining failures found that two of the original diagnoses were wrong.
None of the failures was a product gap. The fixes below change the case set, so rows run
after them are not comparable with the round table above for the changed cases.

- **`mm-dunning-job` (Sonnet).** The earlier diagnosis said the needed memory is scoped to a module that the edited file calls. That is wrong: `src/billing/dunning.py` imported and called nothing.
  - Sonnet passed only when it opened `src/jobs/queue.py` with the Read tool, which fires the file hook. Other runs returned a list of retry dates, which was a fair reading of "Plan the payment retries".
  - **Fix:** `dunning.py` now imports `enqueue`, and the prompt asks for each retry to go on the job queue.
  - **Product gap, not fixed:** the file hook does not fire when Claude reads a file with Bash `cat`, `sed` or `head`.
  - **Possible regression:** Sonnet passed this case 3/3 at baseline and 0–1/3 from v3 on, when it stopped calling `query`. With 3 runs per round this may be noise.
- **`st-retention` (Opus).** Opus was right: no migration added `invoices.finalized_at`, and `refunds` had no status column, but docs, scripts and a seed memory rely on both. **Fix:** migrations `0001` and `0005` now define them. No `0008` was added, because `nh-other-project` needs it absent.
- **`ds-fx-rounding`, `ds-notifier-flag`.** `no_stale_fact` searched the whole diff, including context lines, removed lines and comments.
  - **Fix:** it now checks only added code: new file paths, plus added lines with comments removed.
  - **Needles:** they name the stale use. For fx it is the `rounding=ROUND_HALF_EVEN` argument. For the notifier it is an import of the root `flags` module.
  - **Effect:** all rounds were re-graded, and exactly the 5 known false failures flipped to pass.
- **Costly-discovery cases.** The earlier claim that they never triggered is wrong. In two Sonnet v4 `dc-seed-dev` runs the rule fired and the save was missing.
  - **The failure count was noisy:** a probe like `ls var` counted as a failure, and a chained `; ls` hid a real one.
  - **The cases were too easy:** every cause was named in the docs or the script, so a model that read first never failed.
  - **Redesign:**
    - The causes are now runtime state or a deep code path, and the docs no longer name the remedies. The runner sets `TZ=Europe/Berlin` and leaves a stale `var/dev.sqlite3`; the backfill zone check moved into `src/billing/periods.py`.
    - The grader measures **discovery cost**: the tool calls from the first failed run of the task command to its first success. Failure is also read from the output. Saving is graded when the cost is 3 or more.
  - Details are in `HARD_CASES.md`.

**Verification run (v4c).** The 6 changed cases, 3 runs per model, on the v4 build with master merged. Cost was $4.36 for Opus and $2.05 for Sonnet.

| Case | Opus | Sonnet |
|---|---|---|
| `mm-dunning-job` | 3/3 | 3/3 (was 0–1/3 from v3 on) |
| `st-retention` | 3/3 (no false creates) | 3/3 |
| 4 costly cases, pass | 12/12 | 11/12 |
| costly saves graded | 2 (both saved, `dc-backfill-tz`) | 1 (not saved, `dc-fx-fixtures`) |

- **The redesign works, but the costly cases still rarely bite.** Only 3 of 24 runs reached the grade: the task command failed, the fix took 3 or more calls, and the agent found the cause.
  - Opus often inspected the runtime state before running. For example, it opened `var/dev.sqlite3`, saw the missing tables and ran `migrate_dev.py` first. It saved the fact anyway, which is not graded.
- **Two grader rules were added after reading these runs:**
  - The save is graded only when a tool call actually used the fact. In two Sonnet `dc-fx-fixtures` runs, Sonnet worked around the missing file: once it wrote the JSON fixture by hand, and once it rewrote `tests/fxdata.py` to read the CSV. It never found `gen_fx_fixtures.py`, so there was nothing to save.
  - For `dc-golden-statements`, only "golden integrity check failed" counts as a failure. The golden diff right after the change is the expected next step.
- **The one real miss:** in Sonnet `dc-fx-fixtures` rep 1, Sonnet failed, found `gen_fx_fixtures.py`, and said in its answer that "docs/testing.md doesn't mention this step". It did not save the fact.
- **A sample of 3 graded runs cannot measure the saving rule.** To measure it, the costly cases need more reps or more cases, not more rounds of this design.


**Costly-discovery run (v4d).** All 6 costly cases (the 4 redesigned and 2 new ones: `dc-reconcile-format`, `dc-search-index-cache`), 10 runs per model, on the v4 build. Cost was $11.97 for Opus and $5.37 for Sonnet.

| | Opus | Sonnet |
|---|---|---|
| Runs | 60 | 60 |
| Runs that found the cause | 60 | 41 |
| Runs graded (costly and found) | 6 | 14 |
| Graded runs that saved the fact | **4/6** | **0/14** |
| Runs with any memory write | 47 | **0** |

- **Sonnet never saves a fact it discovered itself.** It made no memory write in any of the 60 runs, including the 41 where it found the cause. It knows the fact is worth keeping: in one run it ended with "I haven't saved anything to memory. I could record that the backfill needs `TZ=UTC` and a batch size of at most 500, if you want."
  - This is specific to self-discovered facts. When the user states a fact, Sonnet saves it (implicit capture 6/6 in every round).
- **Opus saves eagerly.** It wrote a memory in 47 of 60 runs, mostly after a cheap discovery, which the rule allows but does not require.
  - Its 2 graded misses are `dc-seed-dev`: it ran the migrations and seeded, but did not record that seeding needs `migrate_dev.py` first.
- **Opus is rarely graded.** It investigates efficiently: on the two new cases it compared the file with the script and fixed it in 2 calls. That is below the cost of 3, so those runs are correctly not graded.
- **19 Sonnet runs never found the cause.** Most of them stopped to ask before a destructive or real step, or worked around the problem. Asking before a destructive step is correct behavior. It is not graded.

## Capture round (v6)

v4d showed Sonnet never saving a fact it found itself. ENGRAM.md gained one line, "Save what cost you effort". It says: when a command failed and finding the cause took more than one try, and nothing in the repo or the error message states that cause, `create` a hazard before you report back, with the command, the symptom, the cause and the fix. Save it yourself; don't offer to. Skip it when the docs or the error already said what to do.

The gates were written down before the run (`v6/change.md`). All passed.

| Gate | Before | v6 | Verdict |
|---|---|---|---|
| Sonnet saves, graded costly runs (6 cases × 10) | 0/14 | 14/17 (82%) | ≥ 50%: pass |
| Opus saves, graded costly runs | 4/6 | 7/7 | not lower: pass |
| Sonnet full set: pass, no false create, $/case | 131–133, 102/102, $0.106 | 133, 102/102, $0.111 (+4.4%) | pass |
| Opus full set: pass, no false create, $/case | 128–130, 96–98/102, $0.231 | 130, 97/102, $0.235 (+1.6%) | pass |

- **Costly cases, pass:** Sonnet 57/60 (was 46/60), Opus 60/60 (was 57/60).
- **Fixture side effects found by this round.** Opus made 3 new false creates, both from the post-climb fixture fixes, not from the rule:
  - `ds-fx-rounding` ×2: the FX fixtures were missing for every case, so this task hit the same discovery. Opus saved a hazard that wrongly said no generator exists.
  - `mm-refund-endpoint` ×1: the new `refunds.status` CHECK allowed only `open`/`settled`, which conflicts with the endpoint's `pending_approval`/`issued` states.
  - **Fix:** the runner now generates the FX fixtures for every case except `dc-fx-fixtures`, and `refunds.status` has no CHECK.
  - **Check (v6f):** the 6 touched cases, 3 runs per model, pass 36/36, with no false creates.
- **Caveat:** the v4/v4b baselines ran on the pre-fix fixture for `st-retention`, `mm-dunning-job` and the costly cases, so those cases are not compared like for like.

**Plugin-only check (v7p).** The same rule was added to the MCP server instructions and the `create` tool description, then measured **without** ENGRAM.md (a plugin-only install). Gates were pre-registered in `v7p/change.md`.

| | Sonnet | Opus |
|---|---|---|
| Saves, graded costly runs (6 cases × 10) | **1/12 (gate ≥ 50%: fail)** | 5/5 (gate ≥ 80%: pass) |
| No false create, `nh-*` and `ds-*` × 3 | 42/42 | 42/42 |

- **The MCP text alone does not move Sonnet.** ENGRAM.md, which is loaded through CLAUDE.md, does (v6: 14/17).
- **The aligned text is kept for consistency.** It causes no false creates and costs nothing measurable, but it has no measured benefit.
- **Reaching plugin-only Sonnet users needs another channel.** See "What I would try next".

## Retried-command prompt (v8)

v7p showed that the MCP text alone does not make Sonnet save in a plugin-only install. v8 puts the reminder in the flow instead:
- **Record:** PostToolUseFailure on Bash records the failed command by a stable key: its script or program, ignoring `VAR=` prefixes, wrappers, flags and probes such as `ls`, `cat` and `grep`.
- **Prompt:** when a later run with the same key succeeds, PostToolUse adds one line asking to save the cause if it is not documented. It does this once per command per session.
- **Piped commands:** `cmd | tail` exits 0 even when `cmd` fails, so a pipeline counts as failed only when its output shows an unmistakable failure.

The gates were written down before the run (`v8/change.md`).

| Gate | Result | Verdict |
|---|---|---|
| Plugin-only, Sonnet saves (6 costly cases × 10) | 20/21, was 1/12 | ≥ 50%: pass |
| Plugin-only, Opus saves | 6/6 | ≥ 80%: pass |
| Plugin-only, no false create on `nh-*`/`ds-*` × 3 | 42/42 on both models | ≥ 95%: pass |
| With ENGRAM.md, Sonnet full set vs v6 | pass 134 (133); no false create 101/102 (102/102); $/case −3.4% | pass |
| With ENGRAM.md, Opus | **pilot**, not the full set: 18 riskiest cases × 2, pass 37/39 | see below |

- **Opus ran a pilot instead of the full set,** at your request. The pilot covers all 6 bug-fix cases, where test fails → fix → test passes, the cases Opus failed before, and the negatives.
  - Both failures are false creates that Opus also made in earlier rounds: `st-pdf-premise` in v4, v4b and v6, and `bf-export-csv` in v4. The prompt did not fire in either run.
  - The prompt fired in 1 of 39 runs, so normal bug-fix loops do not trigger it.
- **Sonnet's one false create** (`st-retention` rep 0) is also not from the prompt, which did not fire in that run.

## Is there room to grow? The xhard pilot

A third case set, `cases_xhard.jsonl` (`XHARD_CASES.md`), was built to look for headroom:
- **Memories:** 114, with groups of near-duplicates (7 rounding rules, 7 retry policies, 7 ownership memories) and six 3-step supersede chains.
- **Cases:** 40, in six categories: crowded retrieval, cross-module, long sessions with a real `/compact` turn, superseded chains, contradiction under noise, and hard negatives.
- **Fixture:** new modules for payouts, subscriptions and an ingest service.
- **Grader:** contradiction cases now also fail when Claude challenges a still-valid neighbour of the target.

Pilot: 20 of the 40 cases, 1 run each, on the v8 build. The cases were weighted toward those expected to be hardest.

| | Sonnet | Opus |
|---|---|---|
| Pass | 19/20 | 18/20 |
| Cost per case | $0.175 | $0.404 |

All three failures are cross-module cases:
- **Opus, `xm-payout-paid` and `xm-cancel-refund`:** Opus read the other modules only with Bash `cat`. The file hook does not fire on Bash reads, and Opus did not `query`. It still used the right facts from the code, but it never consulted memory.
- **Sonnet, `xm-payout-paid`:** Sonnet read files with the Read tool. But the rules it needed (the audit event name and the notifier entry point) are scoped to modules it never opened, so no hook showed them, and it used neither fact.

**Conclusion: little room is left.** Even the hardest-weighted half of xhard passes at 90–95%. The one remaining gap is narrow and well defined: memories of modules that the edited code calls, especially when the files are read through Bash. The crowded, superseded-chain, long-session, contradiction and negative categories all passed on both models.

## What I would try next

- **Cross-module memories.** This is the only gap the xhard pilot found. Two levers, measured on the `xm-*` cases:
  1. run the file hook for paths read with Bash `cat`/`head`/`sed -n`;
  2. when a file is edited, also surface memories scoped to the modules it imports.
- **Run `test_grade.py` in CI.** It has 28 tests and makes no API calls. The grader changed several times after the climb, and one wrong pattern silently changes every pass rate.
- **A full xhard baseline** (40 cases × 3 runs per model) only if one of the levers above is tried. The pilot shows the set is otherwise near its ceiling.
- **Cost:** cut turns, not text (see v5). The remaining memory turns are 0.5–0.8 per case, so the gain is small.

## Where to look

Raw transcripts (`raw/`) and traces (`traces/`) are not on this branch: they are about 450 MB. They are on the branch `eval/memory-tool-use-artifacts`, which the trace links below and in each `report.html` refer to. The tracked `results.jsonl` files hold every graded row, so every number in this report can be recomputed without them; `regrade.py` needs the raw files.


- `.claude/hillclimb/memory-tool-use-hard/{opus,sonnet}/report.html` — per-case scores per round, with links to every trace.
- `.claude/hillclimb/memory-tool-use-hard/narrative.md` — the running summary.
- `.claude/hillclimb/memory-tool-use-hard/{opus,sonnet}/vN/change.md` — each round's reason, prediction and result.
- `evals/memory-tool-use/RESULTS.md` — the original eval, steps 1–7.
- `evals/memory-tool-use/HARD_CASES.md` — the hard case set.
- `evals/memory-tool-use/XHARD_CASES.md` — the xhard case set; pilot results in `.claude/hillclimb/memory-tool-use-xhard/{opus,sonnet}/pilot/`.
