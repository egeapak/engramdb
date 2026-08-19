# Contributor Documentation

For developers hacking on EngramDB itself. To use EngramDB, go to [../users/](../users/) or [../agents/](../agents/).

## Pages

1. [architecture.md](./architecture.md) — layered design, invariants, retrieval pipeline.
2. [code-organization.md](./code-organization.md) — find files by task.
3. [testing.md](./testing.md) — nextest, isolation, the `ml-models` group.
4. [extending.md](./extending.md) — recipes: new embedding provider, MCP tool, memory type, config field, daemon RPC.
5. [embedding-analysis.md](./embedding-analysis.md) — benchmarked study of chunking, field composition, and aggregation.
6. [embedding-model-alternatives.md](./embedding-model-alternatives.md) — model-by-model sweep of the embedding / reranker / NLI / title stack, with latency and footprint.
7. [parallelization-simd.md](./parallelization-simd.md) — the CPU-bound bulk paths: what rayon bought, why an `f32` reduction never auto-vectorizes at any `opt-level`, and how `dot_unit` went from four hand-written `unsafe` backends to one safe `fearless_simd` kernel (with the profile change from `opt-level = "z"` to `3` that made it possible).
8. [turbovec-evaluation.md](./turbovec-evaluation.md) — why the TurboQuant quantized index was measured and rejected for memory vectors, with the harness in [turbovec-probe/](./turbovec-probe/).
9. [query-latency-profile.md](./query-latency-profile.md) — where a query's milliseconds actually go, and why LanceDB fragment count (not row count) sets it.

The CI gate (`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace --all-features`) is enforced — see [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) for the canonical version.

## Repo conventions

- Edition 2021. No nightly.
- Tokio + `async-trait`. `tracing` for logs, not `log`.
- Errors: `thiserror` at module boundaries, `anyhow::Result` at the CLI top level.
- Atomic file writes via `tempfile::NamedTempFile::persist`. Never overwrite in place.
- Comments sparse: document `why`, not `what`.
