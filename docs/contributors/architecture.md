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
   │  crates/engram-cli/  (Clap + dispatch   │    │  engram-cli/src/  │
   │  ─────► src/commands/ handlers)     ◄── │    │  commands/hook.rs │
   │                                         │    └────────┬──────────┘
   └─────────────────┬───────────────────────┘             │
                     │                                     │
   ┌─────────────────┴────────────────┐                   │
   │  crates/engram-mcp/src/server.rs │ ◄─────────────────┘
   │  (rmcp tool surface)             │
   └─────────────────┬────────────────┘
                     │
   ┌─────────────────▼────────────────────────┐
   │  src/ops/  (the engramdb core lib)       │  Typed ops: create/query/update/
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
   │  crates/engram-storage/                  │
   │   - MemoryStore (orchestrator)           │
   │   - LanceDB index (metadata + vectors)   │
   │   - registry, manifest, paths            │
   │   - worktree routing, write locks        │
   └──────────────────────────────────────────┘

           Shared model providers (optional, gated by config):

   ┌────────────────────┐ ┌──────────────────┐ ┌───────────────────────────┐
   │ engram-models/src/ │ │ engram-models/   │ │ src/retrieval/reranker.rs │
   │ embeddings/        │ │ src/nli/onnx.rs  │ │ - BGE / Jina cross-encoder│
   │ - ONNX (default)   │ │ - NLI for        │ └───────────────────────────┘
   │ - Ollama (opt.)    │ │   challenges     │
   └────────┬───────────┘ └────────┬─────────┘
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

The layers are real Cargo workspace crates: `engram-cli` and `engram-mcp` are
front-ends over the `engramdb` core lib (the root crate, `src/`), and the core
re-exports the extracted leaf crates (`engram-storage`, `engram-models`,
`engram-types`, `engram-onnx`) under their historical module paths
(`engramdb::storage`, `::embeddings`, `::types`, …). See
[code-organization.md](./code-organization.md) for the full crate map.

## The one big rule: ops is shared

The CLI and the MCP server are **two surfaces over the same operations**. Every memory operation lives in `src/ops/<name>.rs` with typed input/output structs. The CLI parses Clap args, builds the typed params, calls ops, formats the result via `OutputFormatter`. The MCP server parses MCP tool arguments, builds the typed params, calls ops, serializes the result as MCP tool output.

Concretely, for a single operation like `create`:

```
crates/engram-cli/src/app.rs           # Clap struct for `add`
crates/engram-cli/src/commands/add.rs  # parses args, builds CreateParams, calls ops, formats
crates/engram-mcp/src/server.rs        # parses MCP args, builds CreateParams, calls ops, serializes
src/ops/create.rs                      # the actual logic: validate, embed, persist
```

If you put logic in `crates/engram-cli/src/commands/add.rs` instead of `src/ops/create.rs`, the MCP tool will diverge from the CLI command. This has happened — don't repeat it. The single-ops-impl rule prevents silent skew.

## Storage and LanceDB

`MemoryStore` (in `crates/engram-storage/src/store.rs`) is the orchestrator. It owns:

- the project's `.engramdb/` directory,
- a single LanceDB table holding **both** metadata-for-filtering and vectors,
- the manifest with the embedding fingerprint,
- the advisory file lock for per-project write serialization.

### One table, not two

There is no separate metadata DB. The LanceDB table has both the columns we filter on (`type`, `tags`, `criticality`, `physical`, `logical`, `status`, …) and the vector column. Filtering happens at the LanceDB layer; vector search is the same table with an ANN query. This is why `IndexFilterable` / `VectorMatch` / `IndexSummary` types are in `crates/engram-storage/src/lance_index.rs` — they're the API into that one table.

### Concurrency model

Mutating ops (`create`, `update`, `delete`, `reindex`, `upsert_chunks`, `delete_chunks`) take a **per-project** advisory `flock(2)` from `crates/engram-storage/src/write_lock.rs`. This serializes concurrent writes across processes — multiple Claude Code sessions can safely write to the same store.

Reads are lock-free. LanceDB's MVCC handles concurrent readers.

File writes use atomic temp-then-rename to prevent partial reads — even a hard kill mid-write leaves either the old file or the new file, never half of each.

### Worktree routing

When you run a memory command inside a git worktree, the CLI dispatch (`engram_cli::run`) checks `storage::worktree::resolve_project_root` first. If the cwd is a linked worktree, it:

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

## Epistemic classes and bi-temporal validity

Every memory carries an **epistemic class** (`Epistemic { Fact, Observation, Decision }`, in `crates/engram-types/src/epistemic.rs`) orthogonal to its `MemoryType`. The class answers "what kind of claim is this?" — a fact about the world, an observation of behavior at a point in time, or a choice someone made — and it shapes decay anchoring, challenge penalties, situation weighting, and conflict routing.

Two rules keep this backward compatible:

- **Type-derived defaults are frozen.** `MemoryType::default_epistemic()` (context/convention/relationship/hazard → Fact, debug → Observation, decision/intent/preference → Decision) is the compatibility anchor: pre-epistemic files parse to exactly these classes.
- **Off-diagonal-only emission.** The file writer only emits `epistemic:` (and the validity fields) when they differ from the type-derived default. A memory that never uses the new fields round-trips **byte-identical** to its pre-epistemic form — golden tests in `crates/engram-storage/src/memory_file/tests.rs` enforce this.

Validity is **bi-temporal** (`valid_from`, `invalidated_at`, `superseded_by` in the hidden frontmatter): a memory's window can be closed without deleting it. Windows close via supersession (`create`/`update` with `supersedes`), `resolve --action invalidate`, or consolidation (`compress` invalidates its sources). `update --clear-invalidated` reopens a window. Retrieval and `list` exclude closed-window memories by default (`include_invalidated: true` opts back in); GC reaps them only after `invalidated_retention_days`. A **future-dated** `invalidated_at` is still valid until that instant, mirroring `expires_at`.

The `Validity` struct (`premise`, `invalidated_by`, `origin_task`, `generality`, `derived_from`) records *why* a claim holds and when to re-check it. It normalizes to `None` when empty so the off-diagonal rule above stays byte-exact.

Downstream effects, each in its own layer:

- **Scoring** (`src/scoring/composite.rs`) — a situation multiplier `floor + (1 − floor) × profile[situation][class]` applied after trust; per-class challenge penalties (`ChallengePenalty` in config); facts decay from `verified_at.unwrap_or(created_at)` so re-verification refreshes them.
- **Conflict routing** (`crates/engram-models/src/nli/challenge.rs::route_contradiction`) — the (new, existing) class pair decides whether a detected contradiction challenges the existing memory, the new one, or flags a decision-vs-decision supersession candidate; entrenchment order is trust class → anchor age → confidence.
- **Lifecycle** (`src/ops/task.rs`, `src/ops/compress.rs`) — `task_complete` demotes task-scoped memories to fast decay; telemetry-driven promotion clears `origin_task` for memories re-retrieved across distinct later sessions; consolidation clusters near-duplicate observations (cosine ≥ 0.85 + NLI gate) into a derived Fact and demotes the sources.
- **Doctor** (`src/ops/doctor.rs`) — flags observations whose `watch_paths` changed on disk and facts whose `derived_from` sources were invalidated, using machine-readable origin tags that `ops::verify` clears.

## Open invariants worth knowing

| Invariant | Where it lives | Why |
|-----------|---------------|-----|
| One map for `provider_str → (onnx_spec, ollama_spec)` | `src/ops/mod.rs::provider_specs` | Fingerprint and resolver can't disagree about which model a config selects. |
| `provider_cache_key` includes every model-affecting config field, none of the daemon-only fields | `src/ops/mod.rs::provider_cache_key` | Avoid stale model bundles after a config change. |
| Daemon failures never break operations | `src/daemon/remote.rs` falls back to local providers | Daemon is a perf optimization, not a dependency. |
| All model downloads cache to `dirs::cache_dir() / "engramdb" / "models"` | `crates/engram-storage/src/paths.rs::model_cache_dir` | Restricted-egress environments pre-stage models into one known location. |
| `provider_specs` keys are stable on disk via `model_id()` | `EmbeddingProvider::model_id` | Manifests written today must keep meaning the same model tomorrow. |
| Mutating ops take `flock`, reads are lock-free | `crates/engram-storage/src/write_lock.rs`, `MemoryStore` impls | Cross-process write safety without read penalty. |
| `MemoryType::default_epistemic()` mapping is frozen | `crates/engram-types/src/epistemic.rs` | Pre-epistemic files must keep parsing to the same classes forever. |
| Diagonal memories serialize byte-identical to pre-epistemic files | `memory_file/v2.rs` writer + golden tests | Files untouched by the new fields never churn on rewrite. |
| Plugin manifest and settings fallback register the same hooks | `setup.rs::test_plugin_manifest_hooks_match_settings_fallback` | Both install paths must wire identical hook events. |
