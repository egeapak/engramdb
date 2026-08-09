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
| `crates/engram-cli/src/commands/*.rs::tests` + `src/testutil.rs` | Command-tier `insta` snapshots (tier 1.5, below) |
| `crates/engram-cli/src/progress.rs::tests` | `indicatif` bar rendering, via `InMemoryTerm` |
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

**Everything the CLI prints must go through the formatter.** insta does not
read a stream — `assert_snapshot!` compares a `String` — so a bare `println!`
puts its bytes somewhere no in-process test can reach, permanently and
silently. Use `outln!` / `errln!` / `outraw!` / `errraw!`
(`use crate::output::{outln, …};`), which mirror
`println!`/`eprintln!`/`print!`/`eprint!` including the bare `outln!(f)` form.
The `formatter-output` CI job enforces this; it exempts only `output.rs`, which
defines the macros, and `commands/hook.rs`, which emits the hook protocol
document Claude Code parses off stdout and must never be styled or JSON-wrapped.

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

**Tier 1.5 — the command tier** (`tests` modules in
`crates/engram-cli/src/commands/*.rs`, harness in `src/testutil.rs`, snapshots
under `crates/engram-cli/tests/snapshots/command/`). Runs a command handler
in-process against a real `MemoryStore` in a temp dir, with a scripted
`MockPrompter` and a capturing formatter. This is the only tier that can reach
an interactive flow: tier 1 has no store or prompter, and tier 2 spawns a
binary where `inquire` wants a terminal and `MockPrompter` — `#[cfg(test)]` in
the lib — is invisible to an integration-test crate.

`MockPrompter` records what it was *asked*, which is the point. Prompt wording,
option lists and defaults are user-visible interface that reaches the terminal
through `inquire`, never through the formatter, so nothing asserted them
before. A snapshot pairs the dialogue with the output:

```text
--- prompts ---
? Action: [Keep (reset to Active), Update, …, Skip, Quit] → Skip
--- stdout ---
Kept memory [ID8] as Active.
--- stderr ---
```

Use `TempProject`, `capturing_plain()`, `interaction(&prompter, &cap)` and
`snap_command(name, p.path(), body)`. Names must be unique across the tier and
prefixed with the command — `snap_command` turns insta's module prefix off,
because insta derives it from the *asserting* file and every snapshot would
otherwise be attributed to `testutil`.

Two things reachable only from an injected seam sit alongside this tier. The
`$EDITOR` flows (`add -e`, `update -e`) go to **tier 2** — an editor is just a
child process, so a `#!/bin/sh` script the fixture writes drives the whole
round trip; the module is `#[cfg(unix)]`, matching what CI actually builds off
Linux. The `projects prune` **progress bar** goes to `src/progress.rs`:
`indicatif` draws to the real stderr and hides itself under a pipe, so
construction takes the `ProgressDrawTarget` as a parameter and tests hand it an
`InMemoryTerm`. That needs `indicatif`'s `in_memory` feature, which is a
**dev-dependency only** — it pulls in `vt100`, and resolver v2 keeps
dev-dependency features out of normal builds (`cargo tree -e normal -i vt100`
must stay empty).

**Tier 2 — the binary** (`crates/engram-cli/tests/cli/snapshot/`). Spawns the
real `engramdb` and snapshots one transcript per invocation: command line, exit
code, stdout, stderr. Put anything about *wiring* here — which flag reaches
which renderer, what the exit code is, which stream a message lands on, and
clap's own errors (exit 2, which never reaches `run`). Default format only,
except for the renderer-thin commands (`config`, `stats`, `daemon`, `review`,
`doctor`) that print outside `OutputFormatter` and so are invisible to tier 1.

"Default format" is *JSON*, not pretty: `OutputFormatter::new` falls back to
Json when stdout is not a terminal, and the fixture always pipes. A case that
means to pin the human layout has to pass `--format plain` explicitly —
`snapshot::harvest::list_sessions_plain` is the example, and without it the row
layout is unreachable from this tier.

`snapshot::harvest` needs a corpus rather than just a store, and
`Fixture::write_transcript` builds it: a `.jsonl` under
`$HOME/.claude/projects/<encoded cwd>/`, using the real `encode_project_dir` so
a change to Claude Code's naming breaks the tests instead of quietly making
them search an empty directory. Session ids there are descriptive stems, not
uuids — `normalize` would rewrite a uuid to `[UUID]` and every session would
read alike, so the snapshots could not show which one was listed or marked.
This is deliberately *not* a second copy of `tests/cli/harvest.rs`: that file
owns scope-resolution behaviour and greps for substrings, this one owns the
bytes, the streams and the exit codes.

Tier 2 needs redaction, and `Fixture::normalize` is where it lives. Two rules
about it are easy to get wrong:

- **Do not use `\b` around an id pattern.** Ids appear inside filenames
  (`one-memory_019fd0b6-…`), and `_` is a word character, so `\b` silently
  skips them. Dashed ids are matched unanchored; bare-hex ids capture a
  non-hex delimiter on each side.
- **Anchor short id patterns to a context.** The command tier learned this the
  hard way twice: eight hex characters occur in prose (`deadbeef` is a legal
  summary), so its 8-char rules match `^ID: …` and `memory …` rather than a
  bare run — and both must run *after* the full-uuid rule, because memories
  created in the same second share a uuid-v7 prefix, so an unanchored rule bites
  a different memory's full id in half and leaks the tail.
- **Redact both spellings of a temp dir.** `RegistryBackend::update` stores
  `dir.canonicalize()` and commands echo it back, while `TempDir::path()` is
  uncanonicalised. They are equal on Linux; on macOS `/tmp` → `/private/tmp`
  and the real path lands in the snapshot. Replace longest-first — the raw path
  is a prefix of the canonical one.
- **Pin model configuration, do not rely on absence.** Whether
  `libonnxruntime` is installed differs between a laptop and CI, and a missing
  one prints a warning. `fixture_config()` disables rerank/NLI and selects the
  keyword titler so availability is irrelevant. It is built from
  `EngramConfig::default()` and serialized whole — a hand-written *partial*
  table fails to deserialize (several fields have no serde default) and
  `load_config_or_default` quietly substitutes defaults.
- **Redact anything generated fresh per render.** `harvest show` frames the
  recorded transcript with a random fence token, deliberately, so recorded
  content cannot forge the framing — three occurrences per digest, 32 undashed
  hex characters, caught by neither the UUID nor the project-id rule. A value
  like this is a guaranteed flake rather than a possible one: the snapshot
  passes on the run that records it and fails on the next. This is what "always
  run a snapshot suite twice" is for.
- **A live service is a machine report too.** The fixture pins model
  *availability*, but not which backend `Auto` lands on, so `harvest index` /
  `harvest search` end in an Ollama socket error whose wording is the OS's
  (`os error 111` on Linux, `61` on macOS, different again with Ollama actually
  running). Redacted to `[EMBEDDING_UNAVAILABLE]` after the command's own
  framing, which is the part that is a contract. `setup` had the same shape and
  no redaction: it probes for the Claude CLI by running `claude --version`, so
  the snapshot recorded whichever branch the recording machine happened to
  have, passed locally and failed on CI. `Fixture::base` now strips `claude`
  from `PATH` alongside `engramdb`, and `setup_dry_run_with_claude_cli` plants a
  stub to cover the other branch on purpose.
- **Structural redaction has to survive JSON-lines.** What config cannot pin,
  `render_stdout` redacts by parsing — `doctor`'s ONNX Runtime row reports
  *where* `libonnxruntime` was loaded from, and the passing and failing forms
  differ in shape (`status`/`suggestion` exist only on the failure), so no
  line-level rule can square them. The trap is that several commands print more
  than one JSON document: `doctor --fix` emits the report and then a
  `{"message":…}` line per action, so a single `from_str` over the whole of
  stdout fails and the redaction is skipped entirely. That shipped once — the
  snapshot held the accepting machine's `/tmp/onnxruntime-…` path and CI failed
  on `/usr/local/lib/…`. `redact_json_lines` handles the multi-document case,
  leaving each document's bytes alone unless redaction changed it. When you
  snapshot a command that prints JSON, check whether it prints *one*.

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
