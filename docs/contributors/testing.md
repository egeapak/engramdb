# Testing

EngramDB requires `cargo-nextest`. Two reasons:

1. **Process-per-test isolation.** The `engram-test-support` crate provides a `#[ctor::ctor]` arm that points `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` at per-process temp dirs (the core's `src/lib.rs` links it under `#[cfg(test)]`; downstream crates pull it in as a dev-dependency). With `cargo test`, the ctor fires once for the whole process and isolation breaks.
2. **Per-group thread caps.** `.config/nextest.toml`'s `ml-models` group runs ONNX-model tests serially (`max-threads = 1`) so they don't thrash RAM.

## Running tests

Always pass `--workspace` — without it, only the crate in the current directory is tested.

```bash
# Full suite — what CI runs
cargo nextest run --workspace --all-features

# One crate in isolation (fast iteration; engram-types has no heavy deps)
cargo nextest run -p engram-types

# Core-lib tests only
cargo nextest run -p engramdb --lib

# One module
cargo nextest run --workspace --all-features -E 'test(retrieval::engine::tests::)'

# One specific test by exact name
cargo nextest run --workspace --all-features -E 'test(=retrieval::engine::tests::test_search_with_real)'

# Doctests (nextest doesn't run them; run separately if you have them)
cargo test --doc
```

See nextest's docs for the full filter-expression grammar.

**Don't run `cargo test --lib`** — isolation breaks (see above). The two flaky tests under that mode (`ops::doctor::tests::test_doctor_many_memories_healthy`, `ops::projects::tests::test_get_project_info_with_memories`) are documented in CLAUDE.md as not regression signals.

## ML-model tests

`.config/nextest.toml`:

```toml
[test-groups.ml-models]
max-threads = 1

[[profile.default.overrides]]
filter = "test(nli::onnx::tests::) | test(embeddings::onnx::tests::) | test(retrieval::engine::tests::test_rerank) | test(retrieval::engine::tests::test_search_with_real)"
test-group = "ml-models"
```

These tests:

- load real ONNX models (NLI, embeddings, reranker),
- need disk space for the cached models,
- need network on first run unless the cache is pre-populated.

When adding a new test that loads a real model, **add it to the `ml-models` group** in `nextest.toml`. Otherwise it'll race with the existing model tests and explode.

For restricted-egress sandboxes (no `cdn.pyke.io` / `huggingface.co`), see the pre-staging workarounds in [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) under "Building & testing in Claude Code on the web".

## Where tests live

| Location | Kind |
|----------|------|
| `tests` modules colocated in each source file (in `src/` and every `crates/*/src/`) | Unit tests, colocated with the code |
| `src/daemon/tests.rs` | Daemon integration tests (in-process Unix socket) |
| `crates/engram-cli/tests/cli/*.rs` | Black-box CLI tests using `assert_cmd` |
| `crates/engram-cli/tests/title_integration.rs` | Title generation integration |
| `benches/` | Criterion benches (run with `cargo bench`) |

### CLI tests

`crates/engram-cli/tests/cli/` builds the `engramdb` binary and shells out to it via `assert_cmd`. Each test gets its own temp dir for the project, and the global config / data dirs are isolated via env-var override in the test harness.

`crates/engram-cli/tests/cli/helpers.rs` has shared setup. New CLI tests should use the helpers, not re-implement temp-dir setup.

### Snapshot/golden tests

There are none. Output assertions are inline. Propose on a PR before adding a snapshot dependency.

## Adding tests for ML-backed code

When a test needs to load a real model:

1. Add it to a `tests` module in the relevant source file (e.g. `crates/engram-models/src/embeddings/onnx.rs::tests`).
2. Make sure it's matched by the `ml-models` filter in `nextest.toml` — either by being in one of the already-matched modules, or by extending the filter expression.
3. Skip gracefully if the model can't load (e.g. `let Some(provider) = OnnxProvider::try_new() else { return; }`). Some CI/sandboxes can't download the model on first run.

The reranker/NLI tests follow this pattern — look at them for a working example.
