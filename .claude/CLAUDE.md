# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

EngramDB is a project-scoped persistent memory store for coding agents.
Tech stack: Rust (edition 2021), LanceDB (vector index), ONNX Runtime via `fastembed` / raw `ort` (all-MiniLM-L6-v2 embeddings, BGE reranker, NLI), MCP protocol via `rmcp`, Tokio.

The repo ships **one binary** (`engramdb`, `src/main.rs`) that does everything — CLI, `serve` for the MCP server, `daemon` for the shared embedding host, and `hook` for Claude Code hook handlers. The same binary is what the Claude Code plugin in `.claude-plugin/` wires up.

## Code Quality (mandatory)

Before marking ANY task as complete, you MUST run and pass both:

1. **`cargo fmt --all`** — format all code. Run this first.
2. **`cargo clippy --all-targets --all-features -- -D warnings`** — all clippy warnings are treated as errors. Fix every warning before proceeding.

No task is done until both commands succeed with zero warnings and zero errors. This applies to all agents and subagents. CI (`.github/workflows/ci.yml`) runs the exact same two commands plus `cargo nextest run --all-features`.

## Common commands

```bash
# Build (debug / release)
cargo build
cargo build --release

# Run the full test suite — nextest is required, not `cargo test`
cargo nextest run --all-features

# Library tests only (matches the flaky-test caveat below)
cargo nextest run --lib

# Run a single test by exact name
cargo nextest run --all-features -E 'test(=retrieval::engine::tests::test_search_with_real)'

# Run all tests in one module
cargo nextest run --all-features -E 'test(retrieval::engine::tests::)'

# Format / lint (CI gates)
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings

# Benches (Criterion)
cargo bench

# Examples (under examples/)
cargo run --example onnx_bench
cargo run --example embed_quality
```

Feature flags from `Cargo.toml`:

- `ollama` (default) — Ollama embedding backend via `reqwest`. Disable for pure offline ONNX.
- `coreml` (macOS only) — Apple Neural Engine EP for `ort`.
- `xnnpack` — portable CPU-kernel EP for A/B benchmarking.

### Nextest test groups

`.config/nextest.toml` puts ONNX-model-loading tests (`nli::onnx::tests::*`, `embeddings::onnx::tests::*`, `retrieval::engine::tests::test_rerank`, `test_search_with_real`) in the `ml-models` group with `max-threads = 1`. Don't parallelize these — they share heavyweight model state.

### Test isolation

`src/lib.rs` installs a `#[ctor::ctor]` that points `ENGRAMDB_DATA_DIR` and `ENGRAMDB_CONFIG_DIR` at per-process temp dirs so tests never touch the real `~/Library/Application Support/engramdb/` or registry. Nextest's process-per-test model is load-bearing here — `cargo test` would not isolate these globals correctly.

Note: `cargo test --lib` has two pre-existing flaky failures under full parallelism (`ops::doctor::tests::test_doctor_many_memories_healthy`, `ops::projects::tests::test_get_project_info_with_memories`) — they pass in isolation and fail identically on a clean base, so they are not a regression signal.

## Building & testing in Claude Code on the web (restricted egress)

The web sandbox's egress gateway uses a custom CA that rustls/webpki-based downloaders (the `ort` build script, `hf-hub`) reject, even though `curl` works (it trusts `/etc/ssl/certs/ca-certificates.crt`). Cold builds/tests fail without these one-time workarounds:

1. **protoc** (LanceDB build dep): `apt-get install -y protobuf-compiler`.
2. **ONNX Runtime binary** (`ort-sys` download fails with `UnknownIssuer`): fetch + decode the prebuilt static lib via curl, then build with `ORT_STRATEGY=system ORT_LIB_LOCATION=/tmp/ort-lib`:
   ```
   curl -sS -o /tmp/ort.tar.lzma2 "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.23.2/x86_64-unknown-linux-gnu.tar.lzma2"
   python3 -c "import lzma; open('/tmp/ort.tar','wb').write(lzma.decompress(open('/tmp/ort.tar.lzma2','rb').read(), format=lzma.FORMAT_RAW, filters=[{'id':lzma.FILTER_LZMA2,'dict_size':1<<26}]))"
   mkdir -p /tmp/ort-lib && tar -xf /tmp/ort.tar -C /tmp/ort-lib
   ```
   Export `ORT_STRATEGY=system` and `ORT_LIB_LOCATION=/tmp/ort-lib` for all `cargo build/clippy/test` commands.
3. **Embedding model** (fastembed download fails the same way): pre-stage `Qdrant/all-MiniLM-L6-v2-onnx` into the hf-hub cache layout under `~/.cache/engramdb/models/models--Qdrant--all-MiniLM-L6-v2-onnx/` with `refs/main` containing `main` and `snapshots/main/<file>` for `model.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`, `tokenizer_config.json` (curl from `https://huggingface.co/<repo>/resolve/main/<file>`). `hf-hub` serves cached files without any network call, so embedding tests then pass offline.

## Architecture

### Layered structure

The codebase is layered so that CLI and MCP share one operations core:

```
CLI (src/cli/)  ─┐
                 ├─► ops (src/ops/) ─► retrieval (engine) ─► storage (MemoryStore + LanceDB)
MCP (src/mcp/) ─┘                  └─► scoring  ─► scope
                                   └─► embeddings / nli / retrieval::reranker
```

- **`src/ops/`** — typed input/output for every memory operation (`create`, `query`, `update`, `delete`, `challenge`, `gc`, `compress`, `reindex`, `doctor`, `stats`, `projects`, …). No CLI formatting, no MCP serialization. Both surfaces call into this same code.
- **`src/cli/`** — Clap definitions (`app.rs`), per-command handlers (`commands/<name>.rs`), and `output.rs` which formats results per `--format pretty|json|plain`. `cli/mod.rs::run` is the dispatch entry; `main.rs` is a 9-line `tokio::main` wrapper.
- **`src/mcp/server.rs`** — single large file implementing the MCP tool surface via `rmcp` macros. Same tool set as CLI subcommands. It owns a `ProviderCache` so the embedding model loads once per process; if the shared daemon is enabled (default), it instead routes inference there via `daemon::remote`.

### Storage (`src/storage/`)

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
- **`src/embeddings/`** — `EmbeddingProvider` trait, implemented by `OnnxProvider` (fastembed; default) and `OllamaProvider` (gated by the `ollama` feature). The `model_id()` method is what gets persisted to the manifest — distinct fp32 vs int8 IDs are required so quantization swaps are detected.
- **`src/nli/onnx.rs`** + **`src/retrieval/reranker.rs`** — optional ONNX NLI for contradiction detection (`challenge` flow) and a cross-encoder reranker (BGE family by default). Both are loaded only when `[nli].enabled` / `[rerank].enabled` are true in `config.toml`.

### Shared embedding daemon (`src/daemon/`)

stdio MCP is one process per agent session, so without coordination every concurrent Claude Code session would load its own copy of the embedding (and optional NLI/reranker) models — hundreds of MB and a ~240ms ONNX init each.

- **`daemon/server.rs`** runs a Unix-domain-socket server that loads each model once machine-wide and serves inference. **Auto-spawned on demand** from the MCP path when `[daemon].enabled = true` (default); race-coordinated by an advisory file lock; exits after `idle_timeout_secs`. The next process that needs it spawns a fresh one.
- **`daemon/remote.rs`** wires `EmbeddingProvider` / `NliProvider` / `Reranker` trait impls that call the daemon over the socket — so the MCP server uses identical seams whether models are local or remote.
- **`daemon/metrics.rs` + persistence** — request metrics are persisted to the global store's LanceDB so `engramdb stats --daemon` reports cumulative figures across restarts (and even when no daemon is running).
- **Socket resolution** (`daemon/mod.rs::resolve_socket`) has fixed precedence: `--socket` flag → `ENGRAMDB_DAEMON_SOCKET` env → `[daemon].socket_path` config → default per-user path under `$XDG_RUNTIME_DIR`/cache dir. **Every** client/server site must use this helper so they agree on the socket.
- **Graceful fallback** is the contract: if the daemon is disabled or unreachable, the MCP process loads models in-process exactly as before. Daemon failures must never break operations.

### Claude Code integration (`src/cli/commands/hook.rs`, `setup.rs`)

- The `engramdb hook pre-tool-use` / `engramdb hook session-start` subcommands are what Claude Code invokes. They read hook event JSON from stdin and emit `additionalContext` JSON to stdout. `SESSION_CONTEXT_BUDGET` (2000 chars) caps the SessionStart injection.
- `engramdb setup` writes the hook + MCP entries into `settings.json` (or under `.claude/` for project scope). The `.claude-plugin/` directory is the marketplace plugin that does the same thing automatically.

### Provider caching

`ProviderCache` in `src/ops/mod.rs` keys cached provider bundles by `provider_cache_key(config, backend_override)` — `backend|provider|dimensions|nli.enabled|nli.model|rerank.enabled|rerank.model`. Daemon-only config fields (e.g. `idle_timeout_secs`) deliberately do **not** affect the key. If you add a new model-affecting config field, you must extend this key or the cache will serve stale bundles after a config change.

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
