Plugin-only check (v7p): the MCP server instructions and the `create` tool description carry the v6 ENGRAM.md wording ("Save what cost you effort ... Save it yourself; don't offer to."). This variant runs WITHOUT ENGRAM.md (plugin-only install), so the MCP text is the only saving instruction.
Why: users who install the plugin without `engramdb setup` get no ENGRAM.md, so the v6 gain did not reach them.
Pre-registered gates:
- Sonnet: costly_capture on the 6 costly cases x10 >= 50% of graded runs (v4d with the old ENGRAM.md wording: 0/14).
- Opus: costly_capture >= 80% of graded runs.
- Guardrail: no_false_create on the create:false negative and distractor cases (nh-*, ds-*) x3 >= 95% per model.
No plugin-only baseline exists on the hard set, so this checks absolute levels, not a delta.
Result: pending.
