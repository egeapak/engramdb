# Contributor Documentation

For developers hacking on EngramDB itself. To use EngramDB, go to [../users/](../users/) or [../agents/](../agents/).

## Pages

1. [architecture.md](./architecture.md) — layered design, invariants, retrieval pipeline.
2. [code-organization.md](./code-organization.md) — find files by task.
3. [testing.md](./testing.md) — nextest, isolation, the `ml-models` group.
4. [extending.md](./extending.md) — recipes: new embedding provider, MCP tool, memory type, config field, daemon RPC.
5. [embedding-analysis.md](./embedding-analysis.md) — benchmarked study of chunking, field composition, and aggregation.
6. [embedding-model-alternatives.md](./embedding-model-alternatives.md) — model-by-model sweep of the embedding / reranker / NLI / title stack, with latency and footprint.
7. [parallelization-simd.md](./parallelization-simd.md) — the CPU-bound bulk paths: what rayon bought, why the release profile (`opt-level = "z"`) defeats auto-vectorization, and why the dot product uses explicit SIMD intrinsics.
8. [turbovec-evaluation.md](./turbovec-evaluation.md) — why the TurboQuant quantized index was measured and rejected for memory vectors, with the harness in [turbovec-probe/](./turbovec-probe/).

The CI gate (`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace --all-features`) is enforced — see [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) for the canonical version.

## Repo conventions

- Edition 2021. No nightly.
- Tokio + `async-trait`. `tracing` for logs, not `log`.
- Errors: `thiserror` at module boundaries, `anyhow::Result` at the CLI top level.
- Atomic file writes via `tempfile::NamedTempFile::persist`. Never overwrite in place.
- Comments sparse: document `why`, not `what`.
