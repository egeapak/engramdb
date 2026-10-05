Review round (v9): fixes from the overfit/strictness review of PR #133.
- Retry prompt: quoted text and heredoc bodies are stripped before keys are made; `-e` is inline code; run-style subcommands keep the script name; a file edit between a failure and the next success settles the failure (no prompt); records are per subagent; the note adds "unless the user asked you not to save memories" and "or if the fix was a change to the code under test".
- ENGRAM.md / MCP text: "Save it yourself rather than offering to, unless the user asked you not to save memories"; challenge non-triggers gain "code the memory itself calls legacy or an exception" and "a source that is not clearly newer than the memory".
- `**/<dir>/...` and `**/<file>` scopes are no longer root-wide; hook_seen is per subagent.
Variants: hard/v9 = with ENGRAM.md, 6 revise:true cases + 8 nh-* cases x2 per model; hard/v9p = plugin-only, 6 costly cases x5, Sonnet; xhard/v9 = ct-* x1 per model.
Pre-registered gates (all graded with the review-fixed grader):
- hard/v9, revise:true cases: revise rate not below v6+v8 on the same cases by more than 1 run per model (challenge text change).
- hard/v9, nh-*: no_false_create and no_spurious_revise 100% (v6/v8 levels).
- hard/v9p Sonnet: costly_capture >= 50% of graded runs (v8p 20/21).
- xhard/v9: pass the ct-* cases with no_collateral_revise on both models.
Result:
- hard/v9 revise:true cases: Sonnet 12/12, Opus 12/12 (v6+v8: 36/36, 22/22). Pass.
- hard/v9 nh-*: no_false_create 16/16 and no_spurious_revise 16/16 on both models. Pass.
- hard/v9p Sonnet (plugin-only): costly_capture 9/10. Pass.
- xhard/v9 ct-*: Opus 4/4. Sonnet 2/4: FAIL. In ct-batch-limit and ct-notifier-retry Sonnet saw the conflict and did not challenge, reasoning that it could not tell whether the source was newer than the memory (hook previews carry no dates). Cause: the new non-trigger "a source that is not clearly newer than the memory". Fixed by flipping the burden: "a source that is clearly older than the memory (an undated source still counts)". Re-checked in v9b.
