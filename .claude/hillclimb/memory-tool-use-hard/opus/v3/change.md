Cost round 1 (v3): inject each memory once per session, in full.
- Hooks skip memories this session's hooks already injected (record at .engramdb/state/hook_seen/<session>; cleared on SessionStart and PreCompact, deleted on SessionEnd). UserPromptSubmit takes a pool of 10 so new memories fill the freed slots.
- `[hooks].preview_chars` 160 -> 1000 (most bodies whole) and `prompt_context_budget` 1500 -> 3000.
Why (measured on v2): cache reads are 77-83% of cost, so turns are the lever. About 2 of ~8 assistant turns per case are memory calls, mostly `get`; 71-76% of gets fetch a memory whose body the hook showed cut short, ~20% one shown title-only, ~2% one shown in full. Half of all hook entries (8-10 per case) repeat a memory already injected in the session.
Prediction: gets 1.4-1.7 -> ~0.3 per case; memory-only turns -1 per case; cost -8 to -13% (Opus 0.245 -> ~0.215, 80% interval 0.200-0.235; Sonnet 0.118 -> ~0.104, 0.097-0.113); hook text per case not higher than v2 (6.2k / 7.0k chars).
Falsifier: if memory-only turns drop by less than 0.5 per case, the lever is dead.
Gates (pre-registered): pass within 3 runs of v2 (whole and test), guardrails within 3 runs, cost at least 5% lower, mechanism visible.
