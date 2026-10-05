Capture round (v6): ENGRAM.md gains a concrete saving trigger: "Save what cost you effort — when a command failed and finding the cause took more than one try, and nothing in the repo or the error message states that cause, `create` a hazard before you report back: the command, the symptom, the cause and the fix. Save it yourself; don't offer to. Skip it when the docs or the error already said what to do." Only ENGRAM.md changes; the binary and MCP instructions are the v4 build.
Why: in v4d (6 costly cases x10) Sonnet saved 0/14 graded discoveries and made no memory write in 60 runs, while saving user-stated facts 6/6. The old line ("after discovering patterns, decisions, hazards, or conventions") reads as design knowledge, and Sonnet offered to save instead of saving.
Pre-registered gates:
- Primary (Sonnet): costly_capture on the 6 costly cases x10 >= 50% of graded runs (was 0/14).
- Opus: costly_capture not below v4d (4/6).
- Guardrails, full hard set x3 vs the mean of v4 and v4b on the 45 climb cases: no_false_create not worse by more than 2 runs; whole pass not worse by more than 3 runs; $/case not up by more than 5%.
Result: pending.
