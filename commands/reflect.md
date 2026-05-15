---
description: Reflect on the session and persist durable project/environment/preference learnings to EngramDB
---

You are wrapping up the work you were assigned. Run an EngramDB reflection pass.

Capture only **durable** knowledge — things that will matter for *future*
sessions on this project. Explicitly **skip** minutiae about the task you just
finished (specific line edits, one-off values, transient state).

In scope to remember:

- **Project** — non-obvious architecture, decisions, conventions, hazards,
  footguns, or workflows discovered this session.
- **Environment / tooling** — build, test, CI, or local-setup facts that were
  surprising or hard-won.
- **User preferences** — how the user wants you to work, things they corrected
  you on, or standing instructions they gave.

Steps:

1. **Review existing memories** — call the `query` tool (`mode: "rank"` for the
   areas you touched, or `mode: "filter"` for specific terms) so you update or
   extend memories instead of duplicating them.
2. **Capture new learnings** — for each durable item, call `create` with an
   appropriate type (`decision`, `hazard`, `convention`, `debug`, etc.). If an
   existing memory is now stale, `update` it.
3. **Flag contradictions** — if anything you found contradicts an existing
   memory, call `challenge` on it and surface the conflict to the user.
4. **Optional cleanup** — if there are memories in `NeedsReview` or
   `Challenged` status, offer to `review` them with the user.

If nothing durable came up this session, say so briefly and stop — do not
invent memories. This is a reflection, not a quota.

You can also pull the `memory-session-end` MCP prompt for a
stats-augmented version of this checklist.
