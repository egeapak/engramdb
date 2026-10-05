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
Result: pending.
