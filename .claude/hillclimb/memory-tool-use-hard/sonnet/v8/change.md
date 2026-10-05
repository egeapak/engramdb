Retried-command prompt (v8): a PostToolUse/PostToolUseFailure hook on Bash records a failed command (by script or program, ignoring env, flags and probes such as ls/cat/grep; a pipeline whose output shows an unmistakable failure counts as failed). When a later run of the same command succeeds, it adds one line, once per command per session: "`<cmd>` failed earlier in this session and has now succeeded. If finding the cause took more than one try and nothing in the repo or the error message states it, save it now with `create` ... Save it yourself; don't offer to. Skip this if the docs or the error message already said what to do."
Why: v7p showed the MCP text alone does not move Sonnet in a plugin-only install (1/12 saves). A prompt at the moment of the fix is in the flow, where Sonnet acts.
Variants: v8p = plugin-only (no ENGRAM.md), v8 = with ENGRAM.md (the recommended install).
Pre-registered gates:
- v8p Sonnet: costly_capture on the 6 costly cases x10 >= 50% of graded runs (v7p: 1/12).
- v8p Opus: costly_capture >= 80% (v7p: 5/5).
- v8p guardrail: no_false_create on nh-*/ds-* x3 >= 95% per model (v7p: 42/42).
- v8 guardrail (main risk: a normal bug fix is also fail -> fix -> pass): full hard set x3 vs v6 on the 45 climb cases: no_false_create not worse by more than 2 runs; pass not worse by more than 3 runs; $/case not up by more than 5%.
Result: pending.
