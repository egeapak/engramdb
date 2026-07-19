# Agent Documentation

Reference for AI agents calling EngramDB's MCP tools.

## The 30-second loop

- **Query before answering project questions:** `query` with `mode: "filter"` and relevant keywords.
- **Query before modifying a file:** `query` with `mode: "rank"` and the file path. (PreToolUse hook does this automatically when wired up.)
- **Create after discovering** a non-obvious decision, hazard, convention, or context.
- **Challenge — don't overwrite** — when you find contradictory information.
- **Verify — don't re-create** — when a surfaced memory is confirmed still accurate, `verify` it to refresh its decay.
- **Declare your task** with `task_current` when starting focused work; `task_complete` when it ships, so task-scoped memories fade.

## Pages

1. [workflows.md](./workflows.md) — when to query, create, challenge, verify, task-scope.
2. [query-modes.md](./query-modes.md) — `filter` vs `rank`.
3. [memory-model.md](./memory-model.md) — fields, types, epistemic classes, scoring inputs.
4. [mcp-tools.md](./mcp-tools.md) — every tool, parameter by parameter.
5. [best-practices.md](./best-practices.md) — what makes a memory useful.
