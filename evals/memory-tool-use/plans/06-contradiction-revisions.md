# Plan: revising contradicted memories

Status: planned (not implemented). Source: every failing `revise` run in eval variants baseline–v7.

## Finding: failures are deferrals, and only where the repo backs the memory

- **Where the failures are.** All 29 `revise` failures are in `ct-just` and `ct-loguru`. `ct-rounding` and `ct-release` pass in 48 of 48 runs. Verified by recount.
- **Why those two cases are different.** In both, the fixture still supports the memory:
  - `ct-just`: a Makefile exists and there is no justfile.
  - `ct-loguru`: the code and `pyproject.toml` use structlog.
- **Claude saw the memory and noticed the conflict.** The hook surfaced the memory in 29 of 29 failing runs, and the ID in 6 of 6 from step 3 on. Claude named the conflict in 29 of 29.
- **Then it deferred.** No failing run edited code. Each one checked the repo, found the user's claim unbacked, and said it would update the memory "once you confirm" or "once the justfile is merged".
- **It can do it, just not reliably.** In v6, `ct-just` reps 0 and 1 called `challenge` on the right ID, with the repo state as evidence.
- **The two models revise differently:**
  - Opus mostly calls `create` with `supersedes` (25 of 28 such passes are Opus), sometimes plus `resolve invalidate`.
  - Sonnet mostly calls `update` or `challenge`.

## Grader fixes (before hill-climbing)

1. **Match the full ID.**
   - `_ids_match` and the file-change test in `grade.py` match on `target[:8]`.
   - For UUIDv7 that prefix is a timestamp, which every seed shares, and so do memories created during the same minute.
   - The file test also counts brand-new files. So a plain `create` could pass.
   - It did not happen in any run so far (verified), but it is a false-positive path.
2. **Widen the revising set** for `revise` and `no_spurious_revise`:

   | Tool | Counts? |
   |---|---|
   | `challenge`, `update` | yes |
   | `resolve` with invalidate/update/delete | yes |
   | `create` with `supersedes` naming the target | yes |
   | `delete` | yes |
   | `verify` | no — it confirms the memory |

3. **Tests:**
   - a plain create does not pass;
   - create+supersedes passes;
   - resolve-invalidate passes;
   - verify does not pass;
   - a wrong seed sharing the prefix does not pass.

## Measurement fixes

- **Tag `ct-just` and `ct-loguru` as `repo_disagrees`** and report them apart from repo-neutral cases.
- **Add 6 repo-neutral contradiction cases**, all targeting seeded memories:
  - `ct-finalize`: the ledger now dedupes, so add a retry;
  - `ct-export`: staging bucket repointed;
  - `ct-errors`: problem+json dropped;
  - `ct-flags`: flags moved to LaunchDarkly;
  - `ct-logging-keys`: event keys are camelCase now;
  - `ct-migrations`: migrations are squashed before release now.
- **Run 5 repeats:** 10 cases × 5 = 50 runs per model.

## Steering (in order)

1. **Wording only.** Change the `challenge` description (`crates/engram-mcp/src/server.rs:2393`), the `ChallengeInput` field docs, the server instructions and `ENGRAM.md`. New wording: "When the user says a memory is outdated, challenge it now, with their statement as evidence, even if the code has not caught up yet. A challenge flags the memory; it does not rewrite it."
2. **Only if wording falls short: a contradiction-cue branch in `UserPromptSubmit`.**
   - Cues: "moved off", "no longer", "now use", "instead of", "removed", "changed".
   - When a cue matches and a memory surfaced, add one line naming the top memory's ID.
   - Keep it MCP-agnostic and within `prompt_context_budget`.
   - Gate it on `no_spurious_revise`.
3. **Not recommended:**
   - NLI inside the hook: hooks never load models, `[nli]` is off by default, and detection is not the problem.
   - A `PostToolUse` check: none of the 29 failures made an edit.
