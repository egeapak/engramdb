Retried-command prompt (v8): a PostToolUse/PostToolUseFailure hook on Bash records a failed command (by script or program, ignoring env, flags and probes such as ls/cat/grep; a pipeline whose output shows an unmistakable failure counts as failed). When a later run of the same command succeeds, it adds one line, once per command per session: "`<cmd>` failed earlier in this session and has now succeeded. If finding the cause took more than one try and nothing in the repo or the error message states it, save it now with `create` ... Save it yourself; don't offer to. Skip this if the docs or the error message already said what to do."
Why: v7p showed the MCP text alone does not move Sonnet in a plugin-only install (1/12 saves). A prompt at the moment of the fix is in the flow, where Sonnet acts.
Variants: v8p = plugin-only (no ENGRAM.md), v8 = with ENGRAM.md (the recommended install).
Pre-registered gates:
- v8p Sonnet: costly_capture on the 6 costly cases x10 >= 50% of graded runs (v7p: 1/12).
- v8p Opus: costly_capture >= 80% (v7p: 5/5).
- v8p guardrail: no_false_create on nh-*/ds-* x3 >= 95% per model (v7p: 42/42).
- v8 guardrail (main risk: a normal bug fix is also fail -> fix -> pass): full hard set x3 vs v6 on the 45 climb cases: no_false_create not worse by more than 2 runs; pass not worse by more than 3 runs; $/case not up by more than 5%.
Result, v8 (with ENGRAM.md):
- Sonnet, full set x3 on the 45 climb cases vs v6: pass 134 (133); no_false_create 101/102 (102/102); $/case 0.1070 (0.1107, -3.4%). All gates pass. The one false create (st-retention rep0) is not from the prompt: it did not fire in that run.
- Opus: stopped after 5 full-set runs and replaced by a pilot at the user's request: 18 riskiest cases x2 (all 6 bf-* bug fixes, the cases Opus failed before, negatives). Pass 37/39. Both failures are false creates Opus also made in earlier rounds (st-pdf-premise: v4/v4b/v6; bf-export-csv: v4); the prompt did not fire in either. It fired in 1 of 39 runs, so normal bug-fix loops do not trigger it.
Verdict: kept. The Opus guardrail is a pilot, not the pre-registered full-set gate.
