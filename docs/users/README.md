# User Documentation

For humans operating EngramDB: install it, configure it, run it, hook it into Claude Code, manage projects, swap embedding models, troubleshoot.

## Start here

1. **[installation.md](./installation.md)** — install the binary, verify, understand where files live.
2. **[quickstart.md](./quickstart.md)** — initialize a store, add a memory, query it. Five minutes end-to-end.
3. **[claude-code.md](./claude-code.md)** — wire EngramDB into Claude Code so it activates automatically.

## Reference

- **[cli-reference.md](./cli-reference.md)** — every subcommand and flag of the `engramdb` binary.
- **[configuration.md](./configuration.md)** — the full `config.toml` schema with defaults.

## Concepts

- **[projects-and-worktrees.md](./projects-and-worktrees.md)** — what a project is, the global store, how git worktrees route transparently to the main project.
- **[embeddings.md](./embeddings.md)** — available models, backends, the model-fingerprint guard, how to reindex.
- **[daemon.md](./daemon.md)** — the shared embedding daemon: when it runs, when you'd interact with it, how to disable it.

## When something's wrong

- **[troubleshooting.md](./troubleshooting.md)** — symptom-to-fix index. `engramdb doctor` covers most of it automatically.

---

If you're an AI agent using the MCP tools rather than the CLI, go to [../agents/](../agents/). If you're hacking on EngramDB itself, see [../contributors/](../contributors/).
