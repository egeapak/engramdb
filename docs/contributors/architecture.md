# Architecture

The big picture: how EngramDB's pieces fit together and the invariants that hold across them.

## The layered design

```
       ┌────────────────────────────────────────┐
       │                                        │
       │  Claude Code (or another MCP client)   │
       │                                        │
       └────────────────┬───────────────────────┘
                        │ stdio MCP
                        ▼
   ┌────────────────────┴────────────────────┐    ┌───────────────────┐
   │  src/cli/  ─────► src/cli/commands/ ◄── │    │  src/cli/        │
   │  (Clap + dispatch)                      │    │  commands/hook.rs│
   │                                         │    └────────┬──────────┘
   └─────────────────┬───────────────────────┘             │
                     │                                     │
   ┌─────────────────┴────────────────┐                   │
   │  src/mcp/server.rs               │ ◄─────────────────┘
   │  (rmcp tool surface)             │
   └─────────────────┬────────────────┘
                     │
   ┌─────────────────▼────────────────────────┐
   │  src/ops/                                │  Typed ops: create/query/update/
   │   - typed Params/Result for every op     │  delete/challenge/resolve/gc/
   │   - no formatting, no serialization      │  compress/reindex/doctor/stats/
   │   - one place each op lives              │  projects/...
   └────────┬─────────────────┬────────┬──────┘
            │                 │        │
            ▼                 ▼        ▼
   ┌────────────────┐ ┌─────────────┐ ┌────────────────────┐
   │ src/retrieval/ │ │ src/scoring/│ │ src/scope/         │
   │ - engine       │ │ - composite │ │ - physical/logical │
   │ - filters      │ │ - decay     │ │   proximity        │
   │ - reranker     │ │ - trust     │ └────────────────────┘
   └────────┬───────┘ └──────┬──────┘
            │                │
            └────────┬───────┘
                     ▼
   ┌──────────────────────────────────────────┐
   │  src/storage/                            │
   │   - MemoryStore (orchestrator)           │
   │   - LanceDB index (metadata + vectors)   │
   │   - registry, manifest, paths            │
   │   - worktree routing, write locks        │
   └──────────────────────────────────────────┘

           Shared model providers (optional, gated by config):

   ┌──────────────────┐ ┌────────────────┐ ┌────────────────────────────┐
   │ src/embeddings/  │ │ src/nli/onnx.rs│ │ src/retrieval/reranker.rs  │
   │ - ONNX (default) │ │ - NLI for      │ │ - BGE / Jina cross-encoder │
   │ - Ollama (opt.)  │ │   challenges   │ └────────────────────────────┘
   └────────┬─────────┘ └────────┬───────┘
            │                    │
            ▼                    ▼
   ┌──────────────────────────────────────────┐
   │  src/daemon/  (auto-spawned shared host) │
   │   - server (Unix socket)                 │
   │   - remote providers wired into MCP via  │
   │     the same EmbeddingProvider / Nli /   │
   │     Reranker trait seams                 │
   └──────────────────────────────────────────┘
```

The arrows are dependency direction. The layers above only depend on the layers below.

## The one big rule: ops is shared

The CLI and the MCP server are **two surfaces over the same operations**. Every memory operation lives in `src/ops/<name>.rs` with typed input/output structs. The CLI parses Clap args, builds the typed params, calls ops, formats the result via `OutputFormatter`. The MCP server parses MCP tool arguments, builds the typed params, calls ops, serializes the result as MCP tool output.

Concretely, for a single operation like `create`:

```
src/cli/app.rs               # Clap struct for `add`
src/cli/commands/add.rs      # parses args, builds CreateParams, calls ops, formats
src/mcp/server.rs            # parses MCP args, builds CreateParams, calls ops, serializes
src/ops/create.rs            # the actual logic: validate, embed, persist
```

If you put logic in `src/cli/commands/add.rs` instead of `src/ops/create.rs`, the MCP tool will diverge from the CLI command. This has happened — don't repeat it. The single-ops-impl rule prevents silent skew.

## Storage and LanceDB

`MemoryStore` (in `src/storage/store.rs`) is the orchestrator. It owns:

- the project's `.engramdb/` directory,
- a single LanceDB table holding **both** metadata-for-filtering and vectors,
- the manifest with the embedding fingerprint,
- the advisory file lock for per-project write serialization.

### One table, not two

There is no separate metadata DB. The LanceDB table has both the columns we filter on (`type`, `tags`, `criticality`, `physical`, `logical`, `status`, …) and the vector column. Filtering happens at the LanceDB layer; vector search is the same table with an ANN query. This is why `IndexFilterable` / `VectorMatch` / `IndexSummary` types are in `src/storage/lance_index.rs` — they're the API into that one table.

### Concurrency model

Mutating ops (`create`, `update`, `delete`, `reindex`, `upsert_chunks`, `delete_chunks`) take a **per-project** advisory `flock(2)` from `src/storage/write_lock.rs`. This serializes concurrent writes across processes — multiple Claude Code sessions can safely write to the same store.

Reads are lock-free. LanceDB's MVCC handles concurrent readers.

File writes use atomic temp-then-rename to prevent partial reads — even a hard kill mid-write leaves either the old file or the new file, never half of each.

### Worktree routing

When you run a memory command inside a git worktree, `cli::run` checks `storage::worktree::resolve_project_root` first. If the cwd is a linked worktree, it:

1. Locates the main worktree's project.
2. Ensures the main project is initialized (init it if needed).
3. Registers the current worktree as a sub-project linked to the main.
4. Consolidates any memories the user accidentally wrote to a stray worktree-local `.engramdb/`.
5. Routes the op to the main project.

Five commands are exempt: `init`, `serve`, `completions`, `setup`, `daemon`. They have their own working-directory semantics; auto-routing would silently target the wrong place.

## The model-fingerprint invariant

Every store records a fingerprint of the embedding model it was built with: `model_id()` + `dimensions`, stored in `.engramdb/manifest.toml`. This is checked against the live provider on every store open.

The fingerprint table (`expected_embedding_fingerprint(config)`) and the provider resolver (`resolve_provider(config, backend)`) **both derive from one map**: `provider_specs(provider_str) -> Option<ProviderSpecs>` in `src/ops/mod.rs`. This unification is mandatory.

A "two-map" design — one for fingerprint-to-record, another for what-actually-loads — is a footgun: if they diverge, you can store vectors under one model identity while a different model serves queries. Vectors silently mismatch; search quality degrades; nothing visibly breaks. This was flagged by branch reviewers and fixed by collapsing to one table. Adding a new provider requires updating exactly one place.

## The shared embedding daemon

`engramdb serve` (the MCP server) is one process per agent session. Without coordination, every concurrent session would load its own copy of the embedding (and optional NLI / reranker) models — hundreds of MB each, ~240 ms ONNX init each.

`src/daemon/` solves this:

- **`server.rs`** runs a Unix-domain-socket server that loads each model once and serves inference requests.
- **`remote.rs`** provides `EmbeddingProvider` / `NliProvider` / `Reranker` trait impls that call the daemon over the socket — so the MCP server uses identical seams whether models are local or remote.
- **Auto-spawn.** When MCP needs the daemon and none is reachable, it spawns one (`engramdb daemon run`) detached. Concurrent spawns are race-coordinated by an advisory file lock — only one binds the socket; the others retry.
- **Idle exit.** The daemon exits after `idle_timeout_secs` (default 15 min) with no active connections. The next MCP run spawns a fresh one. **Users never start it manually.**
- **Graceful fallback.** If the daemon is disabled (`enabled = false`) or unreachable for any reason, the MCP server loads models in-process. Daemon failures must never break operations. This is a contract — make sure your daemon changes preserve it.
- **Metrics persistence.** Request counts and latencies are persisted to the global LanceDB store (`src/daemon/metrics.rs`) so `engramdb stats --daemon` reports figures even when no daemon is running and counts stay cumulative across restarts.

Socket resolution (`daemon::resolve_socket`) has fixed precedence: `--socket` flag > `ENGRAMDB_DAEMON_SOCKET` env > `[daemon].socket_path` config > default per-user path. **Every** client/server site must use this helper so they agree on the socket.

## Provider caching

`ProviderCache` (in `src/ops/mod.rs`) caches the loaded model bundles for the life of a process. The cache key is computed by `provider_cache_key`:

```
backend|provider|dimensions|nli.enabled|nli.model|rerank.enabled|rerank.model
```

Daemon-only config fields (`idle_timeout_secs`, `socket_path`) deliberately do **not** affect the key — they don't change the loaded models. If you add a new model-affecting config field, you must extend this key, or the cache will serve stale bundles after a config change. There's a test for this in `provider_cache_tests`.

## Retrieval pipeline

`RetrievalEngine::run` (in `src/retrieval/engine.rs`) is the query pipeline:

1. **Filter** — `apply_index_filters` applies hard filters (`types`, `tags`, `min_criticality`, `status`, `include_expired`) at the LanceDB layer.
2. **Vector search** — if a query is present and embeddings are enabled, fetch top-K candidates by vector similarity from LanceDB.
3. **Scope scoring** — for each candidate, compute physical + logical proximity to the query context.
4. **Composite scoring** — `composite_score` blends semantic similarity, scope proximity, criticality × decay, trust weight, and challenge penalty according to which scoring mode is active (`with_query`, `with_keyword`, `scope_only`, `degraded`).
5. **Rerank (optional)** — if `[rerank].enabled`, the top-`top_n` results are re-scored by a cross-encoder model and blended with the original score.
6. **Threshold + truncate** — apply `relevance_threshold` (filter mode only) and `max_results`.

Two modes, `Filter` and `Rank`, gate stage 1's signal requirement: `Filter` requires at least one of `query`/`path`/`logical`/`tags`; `Rank` does not. The downstream stages are identical.

## Hook handlers

`src/cli/commands/hook.rs` implements `engramdb hook pre-tool-use` and `engramdb hook session-start`. Each reads event JSON from stdin and emits `additionalContext` JSON to stdout. The `SESSION_CONTEXT_BUDGET` constant (2000 chars) caps the SessionStart injection so the prompt doesn't explode.

The hooks are deliberately thin — they're just CLI commands that call into `src/ops/` like everything else. The "magic" is in the plugin (`.claude-plugin/`) and `engramdb setup`, which wire Claude Code's hook system to invoke them.

## Configuration loading

`src/storage/config.rs::load_config` reads `<project>/.engramdb/config.toml` and merges it with the hard-coded defaults from `src/types/config.rs`. Every section uses `#[serde(default)]` so omitting any field falls back to the default. Validation happens during deserialization — invalid values are rejected at load time, not at use time, so a malformed config fails fast.

Env-var overrides (`ENGRAMDB_*`) are read at the call site, not in `load_config`. CLI flag overrides are passed through explicit parameters (`backend_override: Option<EmbeddingBackend>`).

## Open invariants worth knowing

| Invariant | Where it lives | Why |
|-----------|---------------|-----|
| One map for `provider_str → (onnx_spec, ollama_spec)` | `src/ops/mod.rs::provider_specs` | Fingerprint and resolver can't disagree about which model a config selects. |
| `provider_cache_key` includes every model-affecting config field, none of the daemon-only fields | `src/ops/mod.rs::provider_cache_key` | Avoid stale model bundles after a config change. |
| Daemon failures never break operations | `src/daemon/remote.rs` falls back to local providers | Daemon is a perf optimization, not a dependency. |
| All model downloads cache to `dirs::cache_dir() / "engramdb" / "models"` | `src/storage/paths.rs::model_cache_dir` | Restricted-egress environments pre-stage models into one known location. |
| `provider_specs` keys are stable on disk via `model_id()` | `EmbeddingProvider::model_id` | Manifests written today must keep meaning the same model tomorrow. |
| Mutating ops take `flock`, reads are lock-free | `src/storage/write_lock.rs`, `MemoryStore` impls | Cross-process write safety without read penalty. |

If you find yourself wanting to violate one of these, that's the moment to escalate or re-think.
