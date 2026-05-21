# Contributor Documentation

For developers hacking on EngramDB itself. To use EngramDB, go to [../users/](../users/) or [../agents/](../agents/).

## Pages

1. [architecture.md](./architecture.md) — layered design, invariants, retrieval pipeline.
2. [code-organization.md](./code-organization.md) — find files by task.
3. [testing.md](./testing.md) — nextest, isolation, the `ml-models` group.
4. [extending.md](./extending.md) — recipes: new embedding provider, MCP tool, memory type, config field, daemon RPC.

The CI gate (`cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`) is enforced — see [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) for the canonical version.

## Repo conventions

- Edition 2021. No nightly.
- Tokio + `async-trait`. `tracing` for logs, not `log`.
- Errors: `thiserror` at module boundaries, `anyhow::Result` at the CLI top level.
- Atomic file writes via `tempfile::NamedTempFile::persist`. Never overwrite in place.
- Comments sparse: document `why`, not `what`.
