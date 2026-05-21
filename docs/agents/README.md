# Agent Documentation

This is the reference for AI coding agents that call EngramDB's MCP tools. It explains what the tools do, when to use which one, how to phrase queries, and what makes a memory worth storing.

If you've installed the plugin or run `engramdb setup`, every tool below is exposed automatically and you can call them like any other MCP tool.

## Read these in order

1. **[workflows.md](./workflows.md)** — when to query, when to create, when to challenge. This is the most important page.
2. **[query-modes.md](./query-modes.md)** — `filter` vs `rank`. Picking the wrong mode is the most common mistake.
3. **[memory-model.md](./memory-model.md)** — every field on a memory, every type, how they interact with scoring.
4. **[mcp-tools.md](./mcp-tools.md)** — exhaustive tool reference, parameter by parameter.
5. **[best-practices.md](./best-practices.md)** — what makes a memory useful vs noise.

## The 30-second version

- **Before answering a project question** (conventions, workflow, "how do we…"), call `query` with `mode: "filter"` and a query of relevant keywords.
- **Before modifying a file**, call `query` with `mode: "rank"` and the file path. The PreToolUse hook already does this automatically when wired up; you can also call it explicitly.
- **After discovering** an important pattern, decision, or hazard, call `create` to store it.
- **When you find information that contradicts an existing memory**, call `challenge` with evidence. Don't silently overwrite.

That's the loop. Everything below is variations and details on it.

## Quick tool index

| Tool | Mutates? | Purpose |
|------|----------|---------|
| `query` | no | Search / rank memories. The single retrieval entry point. |
| `get` | no | Full content of one memory (including details). |
| `list` | no | List with simple filters and sorting. |
| `stats` | no | Counts and health. |
| `doctor` | no | Store consistency check. |
| `review` | no | List memories needing review (stale or challenged). |
| `compress_candidates` | no | Find memories eligible for compression. |
| `create` | yes | Store a new memory. |
| `update` | yes | Modify an existing memory. |
| `delete` | yes | Remove a memory. Prefer `supersedes` when possible. |
| `challenge` | yes | Flag a memory as potentially wrong. Records evidence; does not delete. |
| `resolve` | yes | Resolve a challenged/needs-review memory: keep, update, or delete. |
| `gc` | yes | Garbage-collect decayed memories. Use `dry_run: true` first. |
| `reindex` | yes | Rebuild the index / vectors. Rare. |
| `compress_apply` | yes | Merge multiple memories into one summary. |
| `projects_list` | no | Discover other projects' IDs. |
| `projects_info` | no | Info on a specific project. |
| `projects_link` | yes | Link a project as a sub-project of another. |
| `projects_unlink` | yes | Promote a sub-project back to a root. |

Full parameter details in [mcp-tools.md](./mcp-tools.md).
