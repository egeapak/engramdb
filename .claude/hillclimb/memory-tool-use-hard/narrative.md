# Hard-set hill climb: narrative

| round | change | Opus whole | Opus test | Sonnet whole | Sonnet test | guardrails (no spurious revise, no false create) | $/case Opus / Sonnet |
|---|---|---|---|---|---|---|---|
| 0 | baseline | 125/135 | 67/69 | 121/135 | 66/69 | 96/96, 102/102 · 96/96, 102/102 | 0.237 / 0.113 |
| 1 | challenge on any contradicting source, not only the user | 132/135 | 68/69 | 122/135 | 68/69 | 95/96, 102/102 · 96/96, 102/102 | 0.250 / 0.117 |

Whole-set numbers are directional: the test split started at 96% on both models, so it can resolve about one case.

Round 1 is the current best on both models. Step 6 of the earlier work had narrowed `challenge` to "the user says a memory is outdated", and both models then named contradictions found in repo files (a dated runbook, a dependency pin) and deferred to the user. Widening the trigger to any contradicting source, with the non-triggers listed next to it, fixed every revise target on Opus and `dc-runbook-export` on Sonnet. The guardrails held once the grader stopped counting challenges of memories the fixture contradicts on purpose (or that a newer seed supersedes) as spurious; that grader fix re-graded every variant and did not change the ranking. Sonnet still misses `st-flask-premise` because the file hook never shows it the directory-scoped memory, which is the round 2 candidate.
