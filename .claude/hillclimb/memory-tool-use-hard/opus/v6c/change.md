Capture round (v6): ENGRAM.md gains a concrete saving trigger: "Save what cost you effort — when a command failed and finding the cause took more than one try, and nothing in the repo or the error message states that cause, `create` a hazard before you report back: the command, the symptom, the cause and the fix. Save it yourself; don't offer to. Skip it when the docs or the error already said what to do." Only ENGRAM.md changes; the binary and MCP instructions are the v4 build.
Why: in v4d (6 costly cases x10) Sonnet saved 0/14 graded discoveries and made no memory write in 60 runs, while saving user-stated facts 6/6. The old line ("after discovering patterns, decisions, hazards, or conventions") reads as design knowledge, and Sonnet offered to save instead of saving.
Pre-registered gates:
- Primary (Sonnet): costly_capture on the 6 costly cases x10 >= 50% of graded runs (was 0/14).
- Opus: costly_capture not below v4d (4/6).
- Guardrails, full hard set x3 vs the mean of v4 and v4b on the 45 climb cases: no_false_create not worse by more than 2 runs; whole pass not worse by more than 3 runs; $/case not up by more than 5%.
Result (all pre-registered gates pass; adopted):
- Sonnet costly cases x10 (v6c): saved 14/17 graded discoveries (82%), was 0/14 in v4d. Pass 57/60 (was 46/60). $/case 0.101 (was 0.089).
- Opus costly cases x10 (v6c): saved 7/7, was 4/6. Pass 60/60 (was 57/60).
- Full set x3 on the 45 climb cases, vs v4 / v4b:
  - Sonnet: pass 133 (131 / 133); no_false_create 102/102 (102 / 102); $/case 0.1107 vs 0.1060 mean, +4.4%.
  - Opus: pass 130 (128 / 130); no_false_create 97/102 (96 / 98); $/case 0.2350 vs 0.2313 mean, +1.6%.
- The 3 new Opus false creates (ds-fx-rounding x2, mm-refund-endpoint x1) came from fixture side effects of the post-climb fixes, not from the rule: the FX fixtures were missing for every case, and refunds.status had a CHECK that conflicted with the refund endpoint. Both fixed; v6f re-runs the touched cases.
- Caveat: the v4/v4b baselines ran on the pre-fix fixture for st-retention, mm-dunning-job and the costly cases, so those cases are not like for like.
