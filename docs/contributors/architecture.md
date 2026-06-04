# Architecture

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

Every store records a fingerprint of the embedding model it was built with: `model_id()` + `dimensions`, stored in `.engramdb/manifest.toml`. The fingerprint table and the provider resolver **both derive from one map**, `provider_specs(provider_str)` in `src/ops/mod.rs`. Adding a new provider requires updating exactly one place — see [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) for the silent-vector-corruption footgun this prevents.

## Provider caching

`ProviderCache` in `src/ops/mod.rs` keys loaded model bundles by `provider_cache_key = backend|provider|dimensions|nli.enabled|nli.model|rerank.enabled|rerank.model`. Daemon-only and routing-only fields (`idle_timeout_secs`, `socket_path`, `use_for_cli`) deliberately don't affect the key. **If you add a model-affecting config field, extend this key** — the `cache_key_is_deterministic_and_signature_sensitive` test will fail if you forget.

## Shared embedding daemon

See [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) ("Shared embedding daemon" section) for the full design. The contract you must not break: **daemon failures never break operations** — when disabled or unreachable, the MCP process loads models in-process. Every client/server site uses `daemon::resolve_socket` so they agree on the socket path.

## Retrieval pipeline

`RetrievalEngine::run` (in `src/retrieval/engine.rs`) is the query pipeline:

1. **Filter** — `apply_index_filters` applies hard filters (`types`, `tags`, `min_criticality`, `status`, `include_expired`) at the LanceDB layer.
2. **Vector search** — if a query is present and embeddings are enabled, fetch top-K candidates by vector similarity from LanceDB.
3. **Scope scoring** — for each candidate, compute physical + logical proximity to the query context.
4. **Composite scoring** — `composite_score` blends semantic similarity, scope proximity, criticality × decay, trust weight, and challenge penalty according to which scoring mode is active (`with_query`, `with_keyword`, `scope_only`, `degraded`).
5. **Rerank (optional)** — if `[rerank].enabled`, the top-`top_n` results are re-scored by a cross-encoder model and blended with the original score.
6. **Threshold + truncate** — apply `relevance_threshold` (filter mode only) and `max_results`.

Two modes, `Filter` and `Rank`, gate stage 1's signal requirement: `Filter` requires at least one of `query`/`path`/`logical`/`tags`; `Rank` does not. Downstream stages are identical.

## Open invariants worth knowing

| Invariant | Where it lives | Why |
|-----------|---------------|-----|
| One map for `provider_str → (onnx_spec, ollama_spec)` | `src/ops/mod.rs::provider_specs` | Fingerprint and resolver can't disagree about which model a config selects. |
| `provider_cache_key` includes every model-affecting config field, none of the daemon-only fields | `src/ops/mod.rs::provider_cache_key` | Avoid stale model bundles after a config change. |
| Daemon failures never break operations | `src/daemon/remote.rs` falls back to local providers | Daemon is a perf optimization, not a dependency. |
| All model downloads cache to `dirs::cache_dir() / "engramdb" / "models"` | `src/storage/paths.rs::model_cache_dir` | Restricted-egress environments pre-stage models into one known location. |
| `provider_specs` keys are stable on disk via `model_id()` | `EmbeddingProvider::model_id` | Manifests written today must keep meaning the same model tomorrow. |
| Mutating ops take `flock`, reads are lock-free | `src/storage/write_lock.rs`, `MemoryStore` impls | Cross-process write safety without read penalty. |
