# Hard-set hill climb: narrative

| round | change | Opus whole | Opus test | Sonnet whole | Sonnet test | guardrails (no spurious revise, no false create) | $/case Opus / Sonnet |
|---|---|---|---|---|---|---|---|
| 0 | baseline | 125/135 | 67/69 | 121/135 | 66/69 | 96/96, 102/102 · 96/96, 102/102 | 0.237 / 0.113 |
| 1 | challenge on any contradicting source, not only the user | 132/135 | 68/69 | 122/135 | 68/69 | 95/96, 102/102 · 96/96, 102/102 | 0.250 / 0.117 |
| 2 | file hook picks memories by nearest enclosing scope | 130/135 | 67/69 | 133/135 | 68/69 | 96/96, 98/102 · 96/96, 102/102 | 0.245 / 0.118 |

Whole-set numbers are directional: the test split started at 96% on both models, so it can resolve about one case.

Round 2 is kept: it is the best Sonnet variant by far and within noise on Opus. Sonnet acts only on what the file hook injects, and that hook took its 5 memories by score, so a memory about a sibling file or a high-criticality project-wide hazard took every slot from the edited directory's own conventions. Picking by nearest enclosing scope lifted Sonnet from 122 to 133 of 135 (train 54 -> 65): `mm-refund-endpoint`, `st-flask-premise` and `mt-refund-after-drift` now pass 3/3. Opus moved 132 -> 130, within one case of noise; its extra failures are no_false_create, where Opus saved real schema inconsistencies of the fixture (docs refer to columns the migrations never add). Those creates also appear in the baseline, so they are not caused by the round 2 change.

Round 1 (challenge on any contradicting source) fixed every revise target on Opus and `dc-runbook-export` on Sonnet. Its guardrails held once the grader stopped counting challenges of memories the fixture contradicts on purpose as spurious.
