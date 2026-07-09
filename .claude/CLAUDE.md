# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

EngramDB is a project-scoped persistent memory store for coding agents.
Tech stack: Rust (edition 2021), LanceDB (vector index), ONNX Runtime via `fastembed` / raw `ort` (all-MiniLM-L6-v2 embeddings, BGE reranker, NLI), MCP protocol via `rmcp`, Tokio.

The repo ships **one binary** (`engramdb`, `crates/engram-cli/src/main.rs`) that does everything — CLI, `serve` for the MCP server, `daemon` for the shared embedding host, and `hook` for Claude Code hook handlers. The same binary is what the Claude Code plugin in `.claude-plugin/` wires up.

This is a **Cargo workspace** (`[workspace] members = ["crates/*"]`). The per-layer crates under `crates/` are, in dependency order:

- `engram-types` — config + shared domain types (leaf, no heavy deps; `cargo check -p engram-types` is ~seconds).
- `engram-onnx` — `ort` execution-provider wiring (re-exported as `engramdb::onnx_ep`).
- `engram-storage` — `MemoryStore`, LanceDB index, paths/registry, telemetry (re-exported as `engramdb::storage` / `::telemetry`).
- `engram-models` — `embeddings` / `nli` / `title` providers.
- `engram-test-support` — the `#[ctor]` test-isolation arm (dev-only).
- `engram-mcp` — the `rmcp` MCP server surface; depends on the core.
- `engram-cli` — Clap CLI + the `engramdb` binary; depends on the core and `engram-mcp`.

The residual **core** is the top-level `engramdb` lib crate (`src/lib.rs`): `daemon`, `ops`, `retrieval`, `scope`, `scoring`, `search`. It re-exports the extracted crates under their historical module paths (`engramdb::storage`, `::embeddings`, `::types`, …) so every `crate::<module>` / `engramdb::<module>` reference keeps resolving unchanged. **The dependency edges only point inward**: the extracted leaf crates must never `use` the core, and `engram-cli`/`engram-mcp` are front-ends that depend on the core (never re-exported from it — that would invert the DAG).

## Code Quality (mandatory)

Before marking ANY task as complete, you MUST run and pass both:

1. **`cargo fmt --all`** — format all code. Run this first.
2. **`cargo clippy --workspace --all-targets --all-features -- -D warnings`** — all clippy warnings are treated as errors. Fix every warning before proceeding.

Always pass `--workspace` so clippy/nextest cover every crate, not just the one in the current directory. No task is done until both commands succeed with zero warnings and zero errors. This applies to all agents and subagents. CI (`.github/workflows/ci.yml`) runs the exact same two commands plus `cargo nextest run --workspace --all-features`.

## Common commands

```bash
# Build (debug / release)
cargo build
cargo build --release

# Run the full test suite — nextest is required, not `cargo test`.
# Always `--workspace`, or only the crate in the cwd is tested.
cargo nextest run --workspace --all-features

# A single crate in isolation (fast iteration; engram-types has no heavy deps)
cargo nextest run -p engram-types
cargo check -p engram-types

# Core-lib tests only (matches the flaky-test caveat below)
cargo nextest run -p engramdb --lib

# Run a single test by exact name
cargo nextest run --workspace --all-features -E 'test(=retrieval::engine::tests::test_search_with_real)'

# Run all tests in one module
cargo nextest run --workspace --all-features -E 'test(retrieval::engine::tests::)'

# Format / lint (CI gates)
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Benches (Criterion)
cargo bench

# Examples (under examples/)
cargo run --example onnx_bench
cargo run --example embed_quality

# Fuzzing (nightly + cargo-fuzz; targets live in fuzz/)
cargo install cargo-fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run memory_file -- -max_total_time=60
```

Feature flags from `Cargo.toml`:

- `onnxruntime` (default) — the ONNX Runtime stack (`ort` + `fastembed` + `engram-onnx`). Turn OFF (with `--no-default-features`) on a platform with no prebuilt ORT and enable `tract` instead. When off, NLI / reranker / T5 titling compile out (all ORT-only); keyword titling remains.
- `tract` — pure-Rust `tract-onnx` embedding backend (fp32 MiniLM). No native ONNX Runtime; the Intel-Mac (`x86_64-apple-darwin`) fallback. Composable with `onnxruntime` (both engines, runtime-chosen) or standalone (`--no-default-features --features tract`). The release workflow's `build-intel` job builds this; `engram-cli/build.rs` warns on a default Intel-Mac build.
- `ollama` (default) — Ollama embedding backend via `reqwest`. Disable for pure offline ONNX.
- `coreml` (macOS only) — Apple Neural Engine EP for `ort` (implies `onnxruntime`).
- `xnnpack` — portable CPU-kernel EP for A/B benchmarking (implies `onnxruntime`).

### Nextest test groups

`.config/nextest.toml` puts ONNX-model-loading tests (`nli::onnx::tests::*`, `embeddings::onnx::tests::*`, `retrieval::engine::tests::test_rerank`, `test_search_with_real`) in the `ml-models` group with `max-threads = 1`. Don't parallelize these — they share heavyweight model state.

### Test isolation

The `engram-test-support` crate provides the `#[ctor::ctor]` arm that points `ENGRAMDB_DATA_DIR` and `ENGRAMDB_CONFIG_DIR` at per-process temp dirs so tests never touch the real `~/Library/Application Support/engramdb/` or registry. Each crate's test build links it (the core's `src/lib.rs` calls `engram_test_support::arm()` under `#[cfg(test)]`; downstream crates pull it in as a dev-dependency). Nextest's process-per-test model is load-bearing here — `cargo test` would not isolate these globals correctly.

The **model cache** is separate from the data dir and is *not* redirected by the `#[ctor]` arm (model-loading tests need the real shared cache). Two env vars (both resolved in `engram_storage::paths`) make model *presence* deterministic for the tests that assert a model is missing/available:

- `ENGRAMDB_MODEL_CACHE_DIR` — overrides `model_cache_dir()` (mirrors `ENGRAMDB_DATA_DIR`). Point it at an empty temp dir to simulate an unstaged cache.
- `ENGRAMDB_OFFLINE` (truthy: `1`/`true`/`yes`/`on`) — makes the embedding/NLI/T5 loaders refuse to download an uncached model, failing fast instead. An empty cache alone is *not* enough on a networked machine (fastembed would just download); pair it with offline. Example: `stats_embeddings_status_onnx_backend_model_missing` sets both so it passes regardless of what the developer has cached.

Note: `cargo test --lib` has two pre-existing flaky failures under full parallelism (`ops::doctor::tests::test_doctor_many_memories_healthy`, `ops::projects::tests::test_get_project_info_with_memories`) — they pass in isolation and fail identically on a clean base, so they are not a regression signal.

Note: `mcp::server::tests::global_retrieve_with_semantic_query` is similarly flaky under a full-parallel `cargo nextest run --all-features` in a resource-constrained sandbox. The daemon path is disabled under `#[cfg(test)]`, so every embedding test loads ONNX in-process; when many processes lose the model-load race at once, the embedding provider resolves to `None`, the memory is stored without a vector, and the semantic query returns empty. It passes in isolation and on adequately-resourced CI, so it is not a regression signal.

### Fuzzing

`fuzz/` is a standalone `cargo-fuzz` crate (its own `[workspace]`, excluded from
default builds). Targets live in `fuzz/fuzz_targets/` and exercise code that
consumes untrusted input — either hand-written parsers or score math whose
inputs originate from on-disk memory files:

- `memory_file` / `memory_file_roundtrip` — TOML/YAML frontmatter + V2 markdown
  via `storage::memory_file::parse_memory_file` / `write_memory_file`.
- `scope_logical` / `scope_physical` / `scope_proximity` — dot-notation LCA math,
  runtime glob compilation, and the top-level physical+logical combiner. All
  assert the score is finite (the [0,1] bound only holds for config-validated
  decay constants, which the fuzzer doesn't respect).
- `decay` — `scoring::decay_factor` age/TTL/half-life arithmetic; asserts a
  finite factor for arbitrary durations and timestamps (finite floor only).
- `registry` — the global `registry.json` (user-writable, read on every
  command) via `serde_json` into `storage::registry::Registry`, then the pure
  hierarchy walks (`resolve_root_project_id`, `collect_descendants`); asserts
  no panic and bounded/cycle-safe traversal.
- `slug` — the filename helpers (`slugify`, `extract_id_from_stem`,
  `stem_matches_id_prefix`) over arbitrary titles/stems; asserts no panic and
  the documented slug bounds (a non-char-boundary slice on multibyte titles
  once panicked here).
- `composite_score` — the core ranking formula; asserts `final_score` is always
  finite even for `NaN`/`inf` criticality (parsed from files via `f64::parse`).

Each target only calls already-`pub` pure functions — no API was widened for
fuzzing. If the surface you want to fuzz is behind a private `mod`, prefer an
existing public entry point that exercises it transitively (e.g. the field
parsers in `storage::memory_file::helpers` are covered through
`parse_memory_file`) rather than making the module `pub`.

**Adding a target:** create `fuzz/fuzz_targets/<name>.rs` *and* register a
matching `[[bin]]` block in `fuzz/Cargo.toml` with `test/doc/bench = false` —
`cargo fuzz build` (no target arg) builds every `[[bin]]`, so a block pointing
at a missing file, or a target file with no block, breaks the whole build.
Drive the target from libfuzzer's `Arbitrary` inputs (tuples of
`String`/`Vec`/`Option`/`i64`/`f64`/`u8` all work out of the box) and construct
domain types from those primitives in the body. For score math, assert the
weakest invariant that must always hold — `is_finite()` — because the fuzzer
ignores config validation; only assert tighter bounds like `[0,1]` when you
first clamp/guard the inputs into their valid domain (see the `floor` guard in
`decay.rs`). When a degenerate input (e.g. a literal `NaN` config field) is an
input-validation concern rather than the arithmetic under test, `return` early
on it so the assertion stays meaningful.

Run with `cargo +nightly fuzz run <target> -- -max_total_time=60`. In the web
sandbox, building requires the same `ORT_STRATEGY=system ORT_LIB_LOCATION=/tmp/ort-lib`
workaround below. A full `fuzz/target` is ~14 GB on top of the main `target`, so
on the web sandbox watch `df -h` and reclaim space with `rm -rf fuzz/target`
(and `cargo clean`) if a build fails with `No space left on device`. Seed inputs
are committed under `fuzz/corpus/<target>/`; the `.gitignore` keeps only curated
`*.md` seeds (the auto-grown binary corpus is ignored). When a crash is found,
commit the failing input from `fuzz/artifacts/` as a new corpus seed so it
becomes a permanent regression case. CI runs these on a schedule via
`.github/workflows/fuzz.yml`, not on every PR.

## Building & testing in Claude Code on the web (restricted egress)

The web sandbox's egress gateway uses a custom CA that rustls/webpki-based downloaders (the `ort` build script, `hf-hub`) reject, even though `curl` works (it trusts `/etc/ssl/certs/ca-certificates.crt`). Cold builds/tests fail without these one-time workarounds:

1. **protoc** (LanceDB build dep): `apt-get install -y protobuf-compiler`.
2. **ONNX Runtime binary** (`ort-sys` download fails with `UnknownIssuer`): fetch + decode the prebuilt static lib via curl, then build with `ORT_STRATEGY=system ORT_LIB_LOCATION=/tmp/ort-lib`. The version must match what the locked `ort` crate expects (`2.0.0-rc.12` → ONNX Runtime 1.24.x / API 24); a mismatch surfaces at *runtime* as "The requested API version [N] is not available". If you swap the lib after a build, also `rm -rf target/debug/build/ort-sys-* target/debug/.fingerprint/ort-sys-* target/debug/deps/*ort_sys*` — the objects are bundled into the ort-sys rlib at its build time, so relinking alone keeps the old runtime:
   ```
   curl -sS -o /tmp/ort.tar.lzma2 "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.24.2/x86_64-unknown-linux-gnu.tar.lzma2"
   python3 -c "import lzma; open('/tmp/ort.tar','wb').write(lzma.decompress(open('/tmp/ort.tar.lzma2','rb').read(), format=lzma.FORMAT_RAW, filters=[{'id':lzma.FILTER_LZMA2,'dict_size':1<<26}]))"
   mkdir -p /tmp/ort-lib && tar -xf /tmp/ort.tar -C /tmp/ort-lib
   ```
   Export `ORT_STRATEGY=system` and `ORT_LIB_LOCATION=/tmp/ort-lib` for all `cargo build/clippy/test` commands.
3. **Embedding model** (fastembed download fails the same way): the default embedding is the **int8-quantized** `DEFAULT_ONNX_EMBEDDING = ONNX_ALL_MINILM_Q` (fastembed `AllMiniLML6V2Q` → repo `Xenova/all-MiniLM-L6-v2`, file `onnx/model_quantized.onnx`), **not** the fp32 `Qdrant/all-MiniLM-L6-v2-onnx`. Stage the quantized repo into `~/.cache/engramdb/models/models--Xenova--all-MiniLM-L6-v2/` with `refs/main` containing `main` and `snapshots/main/<file>` for `onnx/model_quantized.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json` (curl from `https://huggingface.co/<repo>/resolve/main/<file>`). If you also exercise the fp32 path, stage `Qdrant/all-MiniLM-L6-v2-onnx` (file `model.onnx`) the same way. `hf-hub` serves cached files without any network call, so embedding tests then pass offline.

   ⚠️ If only the fp32 `Qdrant` repo is staged, `OnnxProvider::try_new()` (the default-quantized path) returns `None`: embeddings appear unavailable, the `Auto` backend silently falls back to Ollama (unreachable in the sandbox), and ~100 tests fail with `Failed to send embed request to Ollama`. Staging the quantized repo is what fixes that.
4. **T5 title model** (master makes `title.strategy = "t5"` the default; same download failure): pre-stage `DEFAULT_T5_MODEL = T5_XENOVA_Q` (repo `Xenova/t5-small`) into `~/.cache/engramdb/models/models--Xenova--t5-small/` with `refs/main` → `main` and `snapshots/main/<file>` for `onnx/encoder_model_quantized.onnx`, `onnx/decoder_model_quantized.onnx`, `tokenizer.json`. Without it, `create`-path title generation can't build T5.

## Architecture

### Layered structure

The codebase is layered so that CLI and MCP share one operations core:

```
CLI (engram-cli)  ─┐
                 ├─► ops (src/ops/) ─► retrieval (engine) ─► storage (MemoryStore + LanceDB)
MCP (engram-mcp)  ─┘                  └─► scoring  ─► scope
                                   └─► embeddings / nli / retrieval::reranker
```

- **`src/ops/`** — typed input/output for every memory operation (`create`, `query`, `update`, `delete`, `challenge`, `gc`, `compress`, `reindex`, `doctor`, `stats`, `projects`, …). No CLI formatting, no MCP serialization. Both surfaces call into this same code.

**The module graph is a DAG — keep it that way.** Lower layers must never `use crate::<higher>`. Three back-edges were removed to enforce this and must not be reintroduced:
  - The NLI-contradiction challenge flow lives in **`crates/engram-models/src/nli/challenge.rs`** (`challenge_memory`, `challenge_for_contradictions`), not in `ops`, so `retrieval::engine` can drive it without depending up on `ops`. `ops::challenge` is a thin re-export that keeps the `ops::challenge_*` API stable for CLI/MCP.
  - `TitleStrategy` and the `DEFAULT_NLI_MODEL_REPO` default constant live in **`crates/engram-types/src/`** (config values), not in `title`/`nli`. `title` re-exports `TitleStrategy`; a paired test in each of `types::config` and `nli` asserts the NLI repo default never drifts from `nli::DEFAULT_NLI_MODEL.repo`.
  - The daemon health probe is **`daemon::doctor::check_daemon`** (daemon may depend on `ops`, not vice-versa). The CLI builds it and injects it into `ops::doctor_environment(dir, store, daemon_check)`; `ops` tests pass a synthetic check.
- **`crates/engram-cli/`** (crate `engram-cli`, lib `engram_cli`) — Clap definitions (`app.rs`), per-command handlers (`commands/<name>.rs`), and `output.rs` which formats results per `--format pretty|json|plain`. `lib.rs::run` is the dispatch entry; `main.rs` is the `tokio::main` wrapper that owns the `engramdb` binary. Depends on the core (`engramdb`) and `engram-mcp`.
- **`crates/engram-mcp/src/server.rs`** (crate `engram-mcp`, lib `engram_mcp`) — single large file implementing the MCP tool surface via `rmcp` macros. Same tool set as CLI subcommands. It owns a `ProviderCache` so the embedding model loads once per process; if the shared daemon is enabled (default), it instead routes inference there via `daemon::remote`.

### Storage (`crates/engram-storage/src/`, crate `engram-storage`, re-exported as `engramdb::storage`)

- **`MemoryStore`** (`store.rs`) is the orchestrator. Memories live on disk as TOML-frontmatter markdown files in `.engramdb/memories/`. A single LanceDB table (`lance_index.rs`) holds both metadata-for-filtering and (optional) embedding vectors — there is no separate metadata DB.
- **Concurrency:** mutating ops acquire a per-project advisory `flock(2)` (`write_lock.rs`); reads are lock-free and rely on LanceDB MVCC. File writes are atomic temp-then-rename.
- **Paths (`paths.rs`)** resolve platform dirs via the `dirs` crate. Project-local data lives in `<project>/.engramdb/`; **personal** memories and LanceDB indices live in `<global_data_dir>/projects/<id>/{personal,lancedb}/`. `ENGRAMDB_DATA_DIR` and `ENGRAMDB_CONFIG_DIR` override these (the test harness uses them).
- **`project_id.rs` / `registry.rs`** — every project gets a 16-char SHA-256-derived ID; a global `FileRegistry` tracks them. The well-known global store ID starts with underscores so it cannot collide.
- **`worktree.rs`** — when invoked inside a git worktree, `cli::run` transparently routes ops to the **main** worktree's project and registers the worktree as a sub-project. Init/serve/completions/setup/daemon are exempt because they own their own behavior. Don't bypass `resolve_project_root` for normal commands.
- **`manifest.rs` + `EmbeddingFingerprint`** — each store records `model_id()` and dimensions for its embeddings. On open, `expected_embedding_fingerprint(config)` is compared to the live provider; mismatches surface as a `doctor`/warning saying "run `engramdb reindex --embeddings-only`". The fingerprint table and the provider-resolver share **one** `provider_specs` map in `ops/mod.rs` — this unification is intentional (preventing silent vector corruption) and changes to providers must be added in that single map.

### Retrieval & scoring

- **`src/retrieval/engine.rs`** — the `RetrievalEngine` runs queries in two modes: `Filter` (narrow by query/path/logical/tags, query signal required) and `Rank` (rank everything by relevance to a context). Pipeline: index-level filter → optional vector search via LanceDB → composite scoring → optional cross-encoder rerank.
- **`src/scoring/`** — composite score formula depends on mode (see `scoring/mod.rs` doc-comment). Final = `base * scope_multiplier * trust_multiplier - challenge_penalty`, clamped to `[0,1]`. Decay strategies are `None | Linear | Exponential | Step`.
- **`src/scope/`** — physical (file path with depth-decay) + logical (dot-notation hierarchy, max bonus 0.3). Combined score is capped at 1.0.
- **`crates/engram-models/src/embeddings/`** — `EmbeddingProvider` trait, implemented by `OnnxProvider` (fastembed; default, gated by `onnxruntime`), `OllamaProvider` (gated by `ollama`), and `TractEmbeddingProvider` (pure-Rust tract fp32 MiniLM, gated by `tract`; `embeddings/tract.rs`). The `model_id()` method is what gets persisted to the manifest — distinct fp32 vs int8 IDs are required so quantization swaps are detected, and the tract provider's `tract/all-MiniLM-L6-v2-fp32` id is what makes an ONNX↔tract backend swap trigger a `reindex`.
- **`crates/engram-models/src/nli/onnx.rs`** + **`crates/engram-models/src/rerank.rs`** — optional ONNX NLI for contradiction detection (`challenge` flow) and a cross-encoder reranker (BGE family by default). Both are loaded only when `[nli].enabled` / `[rerank].enabled` are true in `config.toml`. The reranker's `Reranker` trait and its `fastembed` loader (`LocalReranker::load`) live in `engram-models` next to the embedding/NLI/T5 loaders (so the core `engramdb` crate carries **no** direct `fastembed` dep); `src/retrieval/reranker.rs` is a thin re-export keeping `crate::retrieval::reranker::{Reranker, RerankScore, LocalReranker}` resolving for the engine, `ops`, and the daemon's `RemoteReranker`.

### Shared embedding daemon (`src/daemon/`)

stdio MCP is one process per agent session, so without coordination every concurrent Claude Code session would load its own copy of the embedding (and optional NLI/reranker) models — hundreds of MB and a ~240ms ONNX init each.

- **`daemon/server.rs`** runs a Unix-domain-socket server that loads each model once machine-wide and serves inference. **Auto-spawned on demand** when `[daemon].enabled = true` (default); race-coordinated by an advisory file lock. Its idle watchdog exits the process after `idle_timeout_secs` with no activity **and** no in-flight connections. Each served request (including `Ping`) refreshes `last_activity`, so heartbeat pings alone keep it resident.
- **`daemon::DaemonCell` + heartbeat (self-heal + session-aware idle).** Provider resolution for both CLI and MCP goes through the shared `daemon::resolve_providers` / `resolve_providers_with` (`src/daemon/resolve.rs` — it lives under `daemon`, not `ops`, because it necessarily reaches daemon internals and `ops` must never depend on `daemon`), backed by a re-resolvable `daemon::DaemonCell` that **replaced the MCP server's old process-lifetime `OnceCell`** (which cached a `None` failure forever). The cell health-checks a cached handle on each resolve and re-spawns a dead daemon — rate-limited to one spawn per `idle/3` window, but only *failed* spawns consume the window (a confirmed-successful spawn resets the timer). Each `serve` process also runs `spawn_daemon_heartbeat`: a background task that resolves via the cell every `max(30s, idle/3)`; each resolve sends a `Ping` that (a) keeps the daemon alive while any session is connected and (b) re-spawns it if it died. So a live MCP session **self-heals** instead of degrading to in-process for its lifetime, and the daemon reaps only after the **last** session disconnects. `DaemonPolicy` (`ConnectOrSpawn | ConnectOnly | InProcess`) expresses the per-front-end difference: MCP uses `ConnectOrSpawn`; the CLI uses `ConnectOnly` by default (opt-in via `[daemon].use_for_cli`, default `true`; `--in-process`/`ENGRAMDB_IN_PROCESS` forces local; `--spawn-daemon` promotes to `ConnectOrSpawn`). The MCP server's daemon branch is still compiled out under `#[cfg(test)]`; daemon-path coverage lives in `src/daemon/tests.rs`.
- **`daemon/remote.rs`** wires `EmbeddingProvider` / `NliProvider` / `Reranker` trait impls that call the daemon over the socket — so the MCP server uses identical seams whether models are local or remote.
- **`daemon/metrics.rs` + persistence** — request metrics are persisted to the global store's LanceDB so `engramdb stats --daemon` reports cumulative figures across restarts (and even when no daemon is running). Heartbeat ping stats (`ping_count` / `last_ping_secs_ago` in `DaemonStatus`, rendered as `pings: N (last Xs ago)`) are **in-memory only** — current daemon, not persisted. `PROTOCOL_VERSION` is `"3"` (the `Status` ping fields were added there; a version mismatch falls back to in-process until the old daemon reaps).
- **Socket resolution** (`daemon/mod.rs::resolve_socket`) has fixed precedence: `--socket` flag → `ENGRAMDB_DAEMON_SOCKET` env → `[daemon].socket_path` config → default per-user path under `$XDG_RUNTIME_DIR`/cache dir. **Every** client/server site must use this helper so they agree on the socket.
- **Graceful fallback** is the contract: if the daemon is disabled or unreachable, MCP **and** the CLI load models in-process exactly as before. Daemon failures must never break operations.

### Claude Code integration (`crates/engram-cli/src/commands/hook.rs`, `setup.rs`)

- The `engramdb hook pre-tool-use` / `engramdb hook session-start` subcommands are what Claude Code invokes. They read hook event JSON from stdin and emit `additionalContext` JSON to stdout. `SESSION_CONTEXT_BUDGET` (2000 chars) caps the SessionStart injection.
- `engramdb setup` writes the hook + MCP entries into `settings.json` (or under `.claude/` for project scope). The `.claude-plugin/` directory is the marketplace plugin that does the same thing automatically.

### Provider caching

`ProviderCache` in `src/ops/mod.rs` keys cached provider bundles by `provider_cache_key(config, backend_override)` — `backend|provider|dimensions|nli.enabled|nli.model|rerank.enabled|rerank.model`. Daemon-only and routing-only config fields (e.g. `idle_timeout_secs`, `use_for_cli`) deliberately do **not** affect the key. If you add a new model-affecting config field, you must extend this key or the cache will serve stale bundles after a config change.

## Model Downloads

All ML model downloads (embeddings, reranker, NLI) MUST cache to the same directory: `dirs::cache_dir() / "engramdb" / "models"` (see `storage::paths::model_cache_dir`).

- Fastembed models: use `InitOptions::new(model).with_cache_dir(cache_dir)`
- HuggingFace Hub models: use `ApiBuilder::new().with_cache_dir(cache_dir).build()`

Never use default cache locations (e.g., `~/.cache/huggingface/hub/`). The web-sandbox workaround above relies on this exact path layout.

## Memory (EngramDB)

This project uses EngramDB as a persistent memory store via MCP.

- **Before answering any project question** (conventions, workflows, architecture, tooling, "how do we..."), call `query` with `mode: "filter"` and a `query` of relevant keywords.
- **Before modifying files**, call `query` with `mode: "rank"` and the file `path` to surface known decisions, hazards, or conventions.
- **After discovering** important patterns, decisions, hazards, or conventions, store them with `create`.
- **If you find contradictory information**, use `challenge` to flag the memory for review.
