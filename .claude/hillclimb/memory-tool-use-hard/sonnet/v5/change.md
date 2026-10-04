Cost round 3 (v5), on top of v4: shrink the pinned tool surface, which every turn re-reads.
- `update` is no longer pinned (`[mcp].always_load` default: query, get, create, challenge). Its schema is 4.2k chars and it was called 9-12 times in 135 runs; `challenge` and `create` with `supersedes` also revise.
- Shorter descriptions of rarely used optional parameters: `project` (19 copies), `audience`, `valid_from`, `title_strategy`. Behavior-carrying text (premise, the challenge and create descriptions, situation) is unchanged.
Measured on v4: EngramDB's static text is ~19k chars (~5k tokens) of an 18-22k-token context per turn; pinned schemas 14.2k chars.
Prediction: cache-read tokens per turn -1.5k to -2.3k; cost -5 to -8% (80% interval -3 to -10%). Falsifier: per-turn cache reads fall by less than 1k tokens.
Gates vs v4: pass within 3 runs (whole and test), guardrails within 3 runs, cost at least 5% lower, mechanism visible (smaller per-turn context; ToolSearch calls for update rare).

Result vs v4: mechanism present (cache-read tokens per turn -2.2k Opus, -1.0k Sonnet), but cost flat: Opus 0.2313 -> 0.2312, Sonnet 0.1063 -> 0.1054 (-0.8%). Cached input is cheap at these prices, so ~12k fewer cache-read tokens per case are worth well under a cent, and small rises in cache writes and output tokens cancelled it. Quality: Opus 128 -> 132/135 (test 65 -> 68), Sonnet unchanged (131/135, test 69/69); within noise. Cost gate failed: not adopted, code reverted. The cost climb stops here (3 cost rounds; v4 adopted).
