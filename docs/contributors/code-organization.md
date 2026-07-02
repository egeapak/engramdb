# Code Organization

For the conceptual layering see [architecture.md](./architecture.md).

## Top-level layout

```
engramdb/
  Cargo.toml             # workspace root + the `engramdb` core lib crate
  src/                   # the core lib: daemon, ops, retrieval, scope, scoring, search
  crates/                # the extracted per-layer crates (workspace members)
  benches/               # criterion benches (benchmarks.rs, daemon_lifecycle.rs)
  examples/              # standalone runnable examples
  fuzz/                  # cargo-fuzz targets (own workspace, excluded from builds)
  .config/nextest.toml   # test-group config (ml-models max-threads=1)
  .github/workflows/     # CI: ci.yml (fmt+clippy+nextest), release.yml, fuzz.yml
  .claude/CLAUDE.md      # repo-level guidance for Claude Code
  .claude-plugin/        # the Claude Code marketplace plugin
  docs/                  # what you're reading
```

This is a **Cargo workspace** (`[workspace] members = ["crates/*"]`). The single
`engramdb` binary lives in `crates/engram-cli/src/main.rs` (a thin `tokio::main`
wrapper around `engram_cli::run`). The root crate is the **core library**
(`engramdb`, `src/lib.rs`), which re-exports the extracted crates under their
historical module paths (`engramdb::storage`, `::embeddings`, `::types`, …) so
`crate::<module>` / `engramdb::<module>` references keep resolving unchanged.

## The crates, in dependency order

Dependency edges only point inward — the leaf crates never `use` the core, and
`engram-cli` / `engram-mcp` are front-ends that depend on the core (never
re-exported from it).

| Crate | Lib name | What lives there |
|-------|----------|------------------|
| `engram-types` | `engram_types` | Config + shared domain types. Leaf, no heavy deps — `cargo check -p engram-types` is ~seconds. |
| `engram-onnx` | `engram_onnx` | `ort` execution-provider selection (CPU/CoreML/XNNPACK). Re-exported as `engramdb::onnx_ep`. |
| `engram-storage` | `engram_storage` | `MemoryStore`, LanceDB index, paths/registry, telemetry. Re-exported as `engramdb::storage` / `::telemetry`. |
| `engram-models` | `engram_models` | `embeddings` / `nli` / `title` providers. Re-exported as `engramdb::embeddings` etc. |
| `engram-test-support` | `engram_test_support` | The `#[ctor]` test-isolation arm (dev-only). |
| `engramdb` (root) | `engramdb` | The core: `daemon`, `ops`, `retrieval`, `scope`, `scoring`, `search`. |
| `engram-mcp` | `engram_mcp` | The `rmcp` MCP server surface. Depends on the core. |
| `engram-cli` | `engram_cli` | Clap CLI + the `engramdb` binary. Depends on the core and `engram-mcp`. |

## Module map

### `crates/engram-types/src/` — data model

```
├── lib.rs
├── config.rs            # EngramConfig + every sub-config; defaults
├── memory.rs            # Memory, MemoryType, Status, Visibility, MemoryUpdate
├── decay.rs             # Decay, DecayStrategy
├── provenance.rs        # Provenance, ProvenanceSource
├── challenge.rs         # Challenge
├── title_strategy.rs    # TitleStrategy (config value; `title` re-exports it)
└── env.rs               # env-var helpers (truthy parsing etc.)
```

### `crates/engram-storage/src/` — disk + LanceDB

```
├── lib.rs               # public re-exports
├── store.rs             # MemoryStore orchestrator (the central type)
├── lance_index.rs       # LanceDB table: schema, IndexEntry, VectorMatch
├── manifest.rs          # manifest.toml: Manifest, EmbeddingFingerprint
├── memory_file/         # frontmatter+markdown serializer (v1 YAML, v2 TOML)
├── paths.rs             # platform paths (project/global/cache/lancedb/...)
├── project_id.rs        # SHA-256-derived 16-char IDs, detect_worktree_main
├── registry.rs          # FileRegistry/InMemoryRegistry, parent-child
├── worktree.rs          # resolve_project_root, consolidate_worktree_into_main
├── write_lock.rs        # per-project advisory flock
├── config.rs            # load_config from config.toml
├── telemetry/           # StatsCollector + LanceDB persistence
├── error.rs             # storage::Result, StorageError
└── test_support.rs      # cfg(test) helpers
```

### `crates/engram-models/src/` — model providers

```
├── lib.rs
├── embeddings/          # EmbeddingProvider trait + EmbeddingError
│   ├── onnx.rs          # OnnxProvider via fastembed
│   ├── ollama.rs        # OllamaProvider via reqwest (gated by `ollama` feature)
│   ├── pool.rs          # provider session pooling
│   └── chunking.rs      # chunk_text (sentence-boundary splitting with overlap)
├── nli/                 # NLI for contradiction detection
│   ├── onnx.rs          # OnnxNliProvider (tokenizers + ort)
│   └── challenge.rs     # challenge_memory, challenge_for_contradictions
└── title/               # automatic memory title generation
    ├── keyword.rs       # RAKE keyword titles
    ├── t5.rs            # T5-based titles
    └── pool.rs          # pooled T5 sessions
```

### `src/` — the core lib (`engramdb`)

```
├── lib.rs               # module declarations + re-exports + test-isolation ctor hook
│
├── ops/                 # the shared operations layer
│   ├── mod.rs           # provider_specs, ProviderCache, embedding_model_report
│   ├── daemon_resolve.rs# DaemonPolicy, re-resolvable DaemonCell, resolve_providers (CLI+MCP)
│   ├── create.rs        # CreateParams, create_memory, validate_summary
│   ├── query.rs         # query_memories, merge_scored_memories
│   ├── update.rs        # UpdateParams, update_memory
│   ├── delete.rs        # delete_memory
│   ├── get.rs           # get_memory
│   ├── list.rs          # ListParams, list_memories, parse_sort_field
│   ├── challenge.rs     # thin re-export of engram-models' nli::challenge (keeps ops::challenge_* stable)
│   ├── resolve.rs       # ResolveParams, resolve_memory
│   ├── review.rs        # ReviewParams, review_memories
│   ├── gc.rs            # gc_memories
│   ├── compress.rs      # compress_candidates, compress_apply
│   ├── reindex.rs       # reindex
│   ├── stats.rs         # compute_stats
│   ├── doctor.rs        # doctor, doctor_environment (the heavy one)
│   ├── maintenance.rs   # automatic main-worktree maintenance pass
│   ├── projects.rs      # registry CRUD: list/info/link/unlink/prune/delete
│   └── parsing.rs       # shared enum/value parsers
│
├── retrieval/           # query pipeline
│   ├── engine.rs        # RetrievalEngine, RetrievalQuery, ScoredMemory (large)
│   ├── filters.rs       # apply_index_filters, SearchFilters
│   └── reranker.rs      # Reranker trait, LocalReranker (BGE/Jina)
│
├── scoring/             # composite scoring
│   ├── mod.rs           # docs of the formula
│   ├── composite.rs     # composite_score, ScoringContext, ScoreBreakdown
│   ├── decay.rs         # decay_factor, effective_relevance
│   └── trust.rs         # trust_weight (Provenance.source → weight)
│
├── scope/               # physical/logical proximity
│   ├── mod.rs           # scope_proximity helper
│   ├── physical.rs      # file-path + glob proximity with depth decay
│   └── logical.rs       # dot-notation hierarchy bonus
│
├── search/              # keyword search
│   └── keyword.rs       # simple weighted tokenized search over summary/content/tags
│
└── daemon/              # shared embedding daemon
    ├── mod.rs           # socket-path resolution
    ├── server.rs        # daemon event loop
    ├── client.rs        # DaemonHandle, query_status, request_shutdown
    ├── protocol.rs      # wire protocol enums + PROTOCOL_VERSION
    ├── transport.rs     # framed Unix-socket transport
    ├── remote.rs        # remote_providers: trait impls calling the daemon
    ├── metrics.rs       # request metrics persisted to LanceDB
    ├── doctor.rs        # check_daemon health probe (injected into ops::doctor_environment)
    └── tests.rs         # daemon integration tests
```

### `crates/engram-mcp/src/` — MCP server surface

```
├── lib.rs
├── server.rs            # EngramDbServer: rmcp #[tool] macros + transports (large)
└── error.rs             # MCP-specific error mapping
```

### `crates/engram-cli/src/` — CLI surface + the binary

```
├── main.rs              # tokio main, calls engram_cli::run
├── lib.rs               # run(): dispatch from parsed Cli into command handlers
├── app.rs               # all Clap structs (Cli, Command, sub-commands)
├── commands/            # one file per subcommand handler
│   ├── add.rs, get.rs, query.rs, list.rs, update.rs, delete.rs,
│   ├── challenge.rs, review.rs, stats.rs, doctor.rs, gc.rs,
│   ├── compress.rs, reindex.rs, migrate.rs, rollback.rs,
│   ├── init.rs, serve.rs, daemon.rs, completions.rs,
│   ├── setup.rs, hook.rs, projects.rs
│   └── mod.rs           # re-exports
├── output.rs            # OutputFormatter: pretty / json / plain rendering
├── prompter.rs          # InquirePrompter for interactive flows
└── validation.rs        # shared validation helpers
```

Black-box CLI integration tests live in `crates/engram-cli/tests/cli/` (see
[testing.md](./testing.md)).

## Where to make changes (by what you want to do)

| You want to… | Edit |
|--------------|------|
| Add a new CLI subcommand | `crates/engram-cli/src/app.rs` (Clap), `crates/engram-cli/src/commands/<new>.rs`, `crates/engram-cli/src/lib.rs` (dispatch), and the `src/ops/<new>.rs` it calls |
| Add a new MCP tool | `crates/engram-mcp/src/server.rs` (the `#[tool]` macro) calling into the same `src/ops/<new>.rs` |
| Change a memory field | `crates/engram-types/src/memory.rs` + `crates/engram-storage/src/lance_index.rs` (schema) + `crates/engram-storage/src/memory_file/` (serializer) + `crates/engram-types/src/config.rs` if it has a default |
| Add a new memory type variant | `crates/engram-types/src/memory.rs::MemoryType` + the default_decay match + `src/ops/parsing.rs::parse_memory_type` |
| Add a new embedding provider | `crates/engram-models/src/embeddings/<provider>.rs` + extend `provider_specs` in `src/ops/mod.rs` (see [extending.md](./extending.md)) |
| Add a new config field | `crates/engram-types/src/config.rs` (with `#[serde(default)]` + a `default_*` fn) + extend `provider_cache_key` if it affects model loading |
| Change scoring | `src/scoring/composite.rs` (formula) or `src/scoring/decay.rs`, `src/scoring/trust.rs` (the multipliers) |
| Change the retrieval pipeline | `src/retrieval/engine.rs` (stages) or `src/retrieval/filters.rs` (LanceDB filter mapping) |
| Add a hook | `crates/engram-cli/src/commands/hook.rs` + `crates/engram-cli/src/app.rs::HookCommand` + plugin manifest `.claude-plugin/plugin.json` + setup writer `crates/engram-cli/src/commands/setup.rs` |
| Add a daemon RPC | `src/daemon/protocol.rs` (wire) + `src/daemon/server.rs` (handler) + `src/daemon/remote.rs` (client) + bump `PROTOCOL_VERSION` if breaking |
| Touch the LanceDB schema | `crates/engram-storage/src/lance_index.rs` (Arrow schema) + a migration in `crates/engram-cli/src/commands/migrate.rs` |

## Keep the DAG a DAG

Lower layers must never `use` a higher layer. Three back-edges were removed to
enforce this and must not be reintroduced:

- The NLI-contradiction challenge flow lives in
  `crates/engram-models/src/nli/challenge.rs`, not in `ops` — `retrieval::engine`
  drives it without depending up on `ops`. `src/ops/challenge.rs` is a thin
  re-export.
- `TitleStrategy` and `DEFAULT_NLI_MODEL_REPO` live in `crates/engram-types/src/`
  (they're config values), not in `title`/`nli`.
- The daemon health probe is `src/daemon/doctor.rs::check_daemon` (daemon may
  depend on `ops`, not vice-versa); the CLI injects it into
  `ops::doctor_environment`.

## Intentionally big files

`crates/engram-mcp/src/server.rs`, `crates/engram-cli/src/app.rs`,
`src/retrieval/engine.rs`, `src/ops/doctor.rs`, `crates/engram-types/src/config.rs`
are large by design — macros, connected algorithms, or `default_*` schemas that
lose cohesion when split. If a split is unavoidable, split along invariants
(e.g. config section), not line count.
