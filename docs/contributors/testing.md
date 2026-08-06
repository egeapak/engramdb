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
| `crates/engram-cli/tests/cli/snapshot/` | Binary-level `insta` snapshots (tier 2, below) |
| `crates/engram-cli/src/output.rs::tests` | Renderer `insta` snapshots (tier 1, below) |
| `crates/engram-cli/tests/title_integration.rs` | Title generation integration |
| `benches/` | Criterion benches (run with `cargo bench`) |

### CLI tests

`crates/engram-cli/tests/cli/` builds the `engramdb` binary and shells out to it via `assert_cmd`. Each test gets its own temp dir for the project, and the global config / data dirs are isolated via env-var override in the test harness.

`crates/engram-cli/tests/cli/helpers.rs` has shared setup. New CLI tests should use the helpers, not re-implement temp-dir setup.

### Snapshot tests

CLI output is covered by `insta` snapshots in **two tiers**. Which one a new
test belongs in depends on what it is trying to pin down.

**Tier 1 — the renderers** (`crates/engram-cli/src/output.rs::tests`, snapshots
under `crates/engram-cli/tests/snapshots/renderer/`). Drives every
`OutputFormatter::print_*` method in-process across `pretty`/`json`/`plain`.
Every input is a literal — pinned ids, pinned clocks, no store, no models — so
these snapshots contain the **real rendered bytes with nothing redacted**. Put
anything about *layout* here: it is faster, and a reviewer reads actual output
instead of placeholders.

`OutputFormatter::capturing()` (test-only) swaps the stdout/stderr sinks for
string buffers. Use `snap_formats(case, |f| …)` for the three-format sweep, and
give any new fixture a fixed timestamp via `fixed(…)` — never `Utc::now()`.

**Colour is a tier-1 concern too.** `snap_colored(case, |f| …)` renders Pretty
with styling forced on and writes a `<case>__pretty_color.snap` next to the
uncoloured twin. Two gates have to be lifted to get an escape out under a test
runner and the helper lifts both: `capturing_colored()` clears the formatter's
own `use_color` flag, and `owo_colors::with_override(true, …)` short-circuits
`if_supports_color`, which would otherwise ask `supports-color` about the real
stdout and find a pipe. The override is a process-global `AtomicU8` — safe only
because nextest gives each test its own process, the same property the
`#[ctor]` env isolation relies on.

Snapshots store readable tags, not raw escapes: `ansi_to_tags` rewrites
`\x1b[32m✓\x1b[39m` as `<green>✓</green>`, because a `.snap` full of control
bytes renders as mojibake in a web diff and as actual colour under `cargo insta
review`. `snap_colored` asserts the render *did* produce escapes — without that
a broken override would leave the colour tests passing on bare text. For the
renderers that stay colourless on purpose, use
`assert_never_styled(case, format, …)`: it takes no snapshot, and instead
asserts the forced-colour render is byte-identical to the ordinary one, which
pins it to bytes `snap_formats` already covers. That is what holds
`--format plain` colourless.

Tier 2 has no colour cases. `OutputFormatter::new` checks `is_tty` itself, so
no env var can force the real binary to style a pipe, and every colour site is
in `output.rs` anyway. The negative direction is already covered — an escape
leaking into redirected output would show up in the tier-2 snapshots.

**Tier 2 — the binary** (`crates/engram-cli/tests/cli/snapshot/`). Spawns the
real `engramdb` and snapshots one transcript per invocation: command line, exit
code, stdout, stderr. Put anything about *wiring* here — which flag reaches
which renderer, what the exit code is, which stream a message lands on, and
clap's own errors (exit 2, which never reaches `run`). Default format only,
except for the renderer-thin commands (`config`, `stats`, `daemon`, `review`,
`doctor`) that print outside `OutputFormatter` and so are invisible to tier 1.

Tier 2 needs redaction, and `Fixture::normalize` is where it lives. Two rules
about it are easy to get wrong:

- **Do not use `\b` around an id pattern.** Ids appear inside filenames
  (`one-memory_019fd0b6-…`), and `_` is a word character, so `\b` silently
  skips them. Dashed ids are matched unanchored; bare-hex ids capture a
  non-hex delimiter on each side.
- **Pin model configuration, do not rely on absence.** Whether
  `libonnxruntime` is installed differs between a laptop and CI, and a missing
  one prints a warning. `fixture_config()` disables rerank/NLI and selects the
  keyword titler so availability is irrelevant. It is built from
  `EngramConfig::default()` and serialized whole — a hand-written *partial*
  table fails to deserialize (several fields have no serde default) and
  `load_config_or_default` quietly substitutes defaults.

`smoke_is_deterministic_across_fixtures` asserts two independent fixtures
produce identical transcripts. If you add a new source of variance, that is
usually what catches it first.

**Working with snapshots:**

```bash
# Review and accept changes interactively
cargo insta review

# Regenerate everything (after an intentional output change)
cargo insta test --accept --test-runner nextest

# Re-run twice; the second run must also pass. Non-determinism shows up here,
# not in the first run.
cargo nextest run -p engram-cli --test cli -E 'test(snapshot::)'
```

Never hand-edit a `.snap`. Under CI, insta's default `Auto` behaviour becomes
`NoUpdate`, so a drifted or missing snapshot **fails the run** rather than
writing a `.snap.new` — there is no way for a stale snapshot to pass silently.

## Adding tests for ML-backed code

When a test needs to load a real model:

1. Add it to a `tests` module in the relevant source file (e.g. `crates/engram-models/src/embeddings/onnx.rs::tests`).
2. Make sure it's matched by the `ml-models` filter in `nextest.toml` — either by being in one of the already-matched modules, or by extending the filter expression.
3. Skip gracefully if the model can't load (e.g. `let Some(provider) = OnnxProvider::try_new() else { return; }`). Some CI/sandboxes can't download the model on first run.

The reranker/NLI tests follow this pattern — look at them for a working example.
