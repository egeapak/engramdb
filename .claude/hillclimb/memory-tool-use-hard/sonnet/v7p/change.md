Plugin-only check (v7p): the MCP server instructions and the `create` tool description carry the v6 ENGRAM.md wording ("Save what cost you effort ... Save it yourself; don't offer to."). This variant runs WITHOUT ENGRAM.md (plugin-only install), so the MCP text is the only saving instruction.
Why: users who install the plugin without `engramdb setup` get no ENGRAM.md, so the v6 gain did not reach them.
Pre-registered gates:
- Sonnet: costly_capture on the 6 costly cases x10 >= 50% of graded runs (v4d with the old ENGRAM.md wording: 0/14).
- Opus: costly_capture >= 80% of graded runs.
- Guardrail: no_false_create on the create:false negative and distractor cases (nh-*, ds-*) x3 >= 95% per model.
No plugin-only baseline exists on the hard set, so this checks absolute levels, not a delta.
Result (plugin-only, no ENGRAM.md):
- Sonnet: saved 1/12 graded costly discoveries (gate >= 50%: FAIL). Costly pass 49/60.
- Opus: saved 5/5 (gate >= 80%: pass). Costly pass 60/60.
- Guardrail: no_false_create on nh-*/ds-* x3 = 42/42 on both models (pass).
Verdict: the MCP text alone does not move Sonnet; ENGRAM.md does (v6: 14/17). The aligned text is kept for consistency: it costs nothing measurable and causes no false creates, but it has no measured benefit. Reaching plugin-only Sonnet users needs a stronger channel, such as a hook at the moment a failed command is followed by a success.
