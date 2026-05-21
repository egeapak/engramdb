# Contributor Documentation

For developers hacking on EngramDB itself: understanding the layered architecture, finding the right module, running the test suite, and extending the system.

If you're looking for how to **use** EngramDB, go to [../users/](../users/) (humans) or [../agents/](../agents/) (AI agents).

## Read these in order

1. **[architecture.md](./architecture.md)** — the big picture: how CLI, MCP, ops, retrieval, storage, and the daemon fit together.
2. **[code-organization.md](./code-organization.md)** — what lives where, and why each module exists.
3. **[testing.md](./testing.md)** — running tests, the nextest setup, ML-model test isolation.
4. **[extending.md](./extending.md)** — adding a new embedding provider, memory type, MCP tool, or scoring weight.

## The CI gate (mandatory)

Every PR must pass these three commands locally before merge — CI runs the identical checks:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --all-features
```

Clippy is `-D warnings` — every warning fails CI. Run `cargo fmt` first, then clippy. See [testing.md](./testing.md) for nextest specifics.

## Repo conventions

- **Edition 2021.** No nightly features.
- **Tokio for async**, `async-trait` for async-in-trait. The CLI entry point is a thin `#[tokio::main]` wrapper around `cli::run`.
- **`tracing` for logs**, not `log`. The CLI installs a `tracing-subscriber` that defaults to `warn` level; override with `RUST_LOG=engramdb=debug`.
- **Errors:** `thiserror` for typed error enums at module boundaries, `anyhow::Result` at CLI / top-level call sites.
- **No `panic!` in library code** except for invariant violations that genuinely indicate a bug.
- **Atomic file writes:** `tempfile::NamedTempFile::persist` — never overwrite in place.
- **Comments are sparse by design.** Document `why`, not `what`. See `CLAUDE.md` for the convention.

## The "one binary" rule

There is one binary, one library crate. The binary's `main.rs` is 9 lines. Every subcommand (CLI, MCP, daemon, hooks) is dispatched from `src/cli/mod.rs::run`. Don't add a separate binary — extend the dispatch.

## The "ops layer is shared" rule

The CLI and the MCP server are both thin facades over `src/ops/`. If you add a memory operation, **the implementation goes in `src/ops/`**, with typed input/output structs. Then thread it through both `src/cli/commands/<name>.rs` and `src/mcp/server.rs`. Don't put business logic in either surface — it belongs in ops.

## The `provider_specs` invariant

`src/ops/mod.rs::provider_specs` is the **single source of truth** for the embedding-model table. The fingerprint that gets persisted to a store and the provider that actually loads at runtime are both derived from this one map. Adding a new model in one place but not the other was a silent-vector-corruption footgun flagged in branch review — the unification is deliberate. See [extending.md](./extending.md) for the exact procedure to add a provider.

## What's where, in two sentences

The CLI lives in `src/cli/`, the MCP server in `src/mcp/`, and they both call `src/ops/` for actual work. `src/storage/` is the disk + LanceDB layer; `src/retrieval/` runs queries against it; `src/scoring/` and `src/scope/` produce the composite score; `src/embeddings/`, `src/nli/`, `src/retrieval/reranker.rs` are the model-backed providers; `src/daemon/` is the optional shared-model host.

Full module-by-module breakdown in [code-organization.md](./code-organization.md).
