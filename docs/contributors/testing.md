# Testing

EngramDB uses `cargo-nextest` for tests, not `cargo test`. There are CI-mandated reasons.

## Why nextest

Two things matter:

1. **Test isolation via process-per-test.** `src/lib.rs` installs a `#[ctor::ctor]` that points `ENGRAMDB_DATA_DIR` and `ENGRAMDB_CONFIG_DIR` at per-process temp dirs. This keeps tests from touching the real `~/Library/Application Support/engramdb/` (or Linux equivalent). Nextest's process-per-test model is **load-bearing** — `cargo test` runs tests in the same process and the ctor only fires once, breaking isolation.
2. **Per-test-group thread caps.** `.config/nextest.toml` declares an `ml-models` group with `max-threads = 1`. Tests that load ONNX models are in that group. They share heavyweight model state; running them in parallel either thrashes RAM or fails outright.

## Running tests

```bash
# Full suite — what CI runs
cargo nextest run --all-features

# Library tests only
cargo nextest run --lib

# One module
cargo nextest run --all-features -E 'test(retrieval::engine::tests::)'

# One specific test by exact name
cargo nextest run --all-features -E 'test(=retrieval::engine::tests::test_search_with_real)'

# Doctests (nextest doesn't run them; run separately if you have them)
cargo test --doc
```

The `-E` flag is nextest's filter expression. Useful patterns:

| Filter | Meaning |
|--------|---------|
| `'test(=foo::bar)'` | Exact test name match. |
| `'test(foo::bar::)'` | Tests in module `foo::bar`. |
| `'test(/test_.*/)'` | Regex over test names. |
| `'kind(test)'` | Only integration tests. |
| `'kind(lib)'` | Only unit tests. |
| `'not test(slow)'` | Negation. |

Combine with `&` / `|` / `not`. Full grammar in nextest's docs.

## Test isolation

```rust
// src/lib.rs
#[cfg(test)]
mod test_isolation {
    static TEST_DATA_DIR: LazyLock<tempfile::TempDir> = LazyLock::new(...);
    static TEST_CONFIG_DIR: LazyLock<tempfile::TempDir> = LazyLock::new(...);

    #[ctor::ctor]
    fn init() {
        std::env::set_var("ENGRAMDB_DATA_DIR", TEST_DATA_DIR.path());
        std::env::set_var("ENGRAMDB_CONFIG_DIR", TEST_CONFIG_DIR.path());
    }
}
```

This runs once per process, **before** main. With nextest, every test gets its own process, so every test gets its own temp dirs. The `TempDir` is held in a static so it persists for the process lifetime and is cleaned up at exit.

**Don't run `cargo test --lib`** on this codebase — it shares one process across all tests and the isolation breaks. Two specific tests fail flakily under that mode:

- `ops::doctor::tests::test_doctor_many_memories_healthy`
- `ops::projects::tests::test_get_project_info_with_memories`

These pass in isolation and fail identically on a clean base. They are **not** regression signals.

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

## Restricted-egress test environments

If you're running tests in a sandbox that can't pull from `cdn.pyke.io` or `huggingface.co` (e.g. Claude Code on the web), you need the pre-staging workarounds documented in `.claude/CLAUDE.md`. The short version:

```bash
# 1. ONNX Runtime binary
curl -sS -o /tmp/ort.tar.lzma2 "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.23.2/x86_64-unknown-linux-gnu.tar.lzma2"
python3 -c "import lzma; open('/tmp/ort.tar','wb').write(lzma.decompress(open('/tmp/ort.tar.lzma2','rb').read(), format=lzma.FORMAT_RAW, filters=[{'id':lzma.FILTER_LZMA2,'dict_size':1<<26}]))"
mkdir -p /tmp/ort-lib && tar -xf /tmp/ort.tar -C /tmp/ort-lib
export ORT_STRATEGY=system ORT_LIB_LOCATION=/tmp/ort-lib

# 2. Embedding model into the hf-hub cache layout
# (Qdrant/all-MiniLM-L6-v2-onnx into ~/.cache/engramdb/models/...)
# Full layout in .claude/CLAUDE.md

# 3. protoc for LanceDB
apt-get install -y protobuf-compiler
```

After that, `cargo nextest run --all-features` should pass offline.

## Where tests live

| Location | Kind |
|----------|------|
| `src/**/tests/` modules in each file | Unit tests, colocated with the code |
| `src/daemon/tests.rs` | Daemon integration tests (in-process Unix socket) |
| `tests/cli/*.rs` | Black-box CLI tests using `assert_cmd` |
| `tests/title_integration.rs` | Title generation integration |
| `benches/benchmarks.rs` | Criterion benches (run with `cargo bench`) |

### CLI tests

`tests/cli/` builds the `engramdb` binary and shells out to it via `assert_cmd`. Each test gets its own temp dir for the project, and the global config / data dirs are isolated via env-var override in the test harness.

`tests/cli/helpers.rs` has shared setup. New CLI tests should use the helpers, not re-implement temp-dir setup.

### Snapshot/golden tests

There are none. Output assertions are inline. If you find yourself wanting snapshots, propose it on a PR before adding the dependency.

## Coverage

There's no enforced coverage threshold. Aim for "every code path that can go wrong has at least one test." For shared `src/ops/` functions, test both:

- the success path (a memory was created/queried/etc.), and
- the failure path (validation rejects bad input).

For storage code (`MemoryStore`, `LanceIndex`), test concurrency: spawn two tasks doing the same op against the same store and verify the flock serializes them correctly.

## CI

`.github/workflows/ci.yml` runs:

```
- cargo fmt --all -- --check
- cargo clippy --all-targets --all-features -- -D warnings
- cargo nextest run --all-features
```

All three must pass for the PR to merge. The clippy step uses `-D warnings` — every warning is an error. Run these locally before pushing.

## Adding tests for ML-backed code

When a test needs to load a real model:

1. Add it to a `tests` module in the relevant source file (e.g. `src/embeddings/onnx.rs::tests`).
2. Make sure it's matched by the `ml-models` filter in `nextest.toml` — either by being in one of the already-matched modules, or by extending the filter expression.
3. Skip gracefully if the model can't load (e.g. `let Some(provider) = OnnxProvider::try_new() else { return; }`). Some CI/sandboxes can't download the model on first run.

The reranker/NLI tests follow this pattern — look at them for a working example.
