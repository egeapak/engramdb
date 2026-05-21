# EngramDB Documentation

EngramDB is a project-scoped persistent memory store for coding agents. It records decisions, hazards, conventions, and context about a codebase so an AI coding agent can pick up where it left off across sessions.

This documentation is organized by audience. Pick the folder that matches what you're doing.

## [users/](./users/) — humans operating engramdb

Install, configure, and run engramdb. Use the CLI, set up the Claude Code plugin, manage projects and worktrees, tune the daemon, swap embedding models, troubleshoot.

- [installation.md](./users/installation.md) — install, prerequisites, verify
- [quickstart.md](./users/quickstart.md) — first store in five minutes
- [cli-reference.md](./users/cli-reference.md) — every subcommand and flag
- [configuration.md](./users/configuration.md) — `config.toml` schema
- [claude-code.md](./users/claude-code.md) — plugin install, hooks, `engramdb setup`
- [daemon.md](./users/daemon.md) — the shared embedding daemon
- [projects-and-worktrees.md](./users/projects-and-worktrees.md) — project IDs, global store, git worktrees
- [embeddings.md](./users/embeddings.md) — backends, models, reindexing
- [troubleshooting.md](./users/troubleshooting.md) — common failures

## [agents/](./agents/) — AI agents using the MCP tools

Reference for an agent calling engramdb's MCP tools — when to query, when to create, how to phrase filter vs rank queries, what each parameter means.

- [mcp-tools.md](./agents/mcp-tools.md) — every tool, parameter-by-parameter
- [query-modes.md](./agents/query-modes.md) — `filter` vs `rank` and when to use which
- [memory-model.md](./agents/memory-model.md) — types, scope, criticality, decay, status
- [workflows.md](./agents/workflows.md) — query-before-answer, store-after-discovery, contradiction loop
- [best-practices.md](./agents/best-practices.md) — what makes a memory useful

## [contributors/](./contributors/) — developing engramdb itself

Internal architecture and how to extend it.

- [architecture.md](./contributors/architecture.md) — layered design, MCP/CLI/ops boundary
- [code-organization.md](./contributors/code-organization.md) — what lives where
- [testing.md](./contributors/testing.md) — nextest, test isolation, ml-models group
- [extending.md](./contributors/extending.md) — adding embedding providers, memory types, MCP tools

---

If you only read one page: pick **users/quickstart.md** for getting set up, **agents/workflows.md** for using engramdb from an agent, or **contributors/architecture.md** for hacking on it.
