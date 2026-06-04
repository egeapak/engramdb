# Code Organization

For the conceptual layering see [architecture.md](./architecture.md).

## Top-level layout

```
engramdb/
  Cargo.toml             # library + bin (both share src/)
  src/                   # all the code
  benches/               # criterion benches
  examples/              # standalone runnable examples
  tests/                 # integration tests
  .config/nextest.toml   # test-group config (ml-models max-threads=1)
  .github/workflows/     # CI: ci.yml (fmt+clippy+nextest), release.yml
  .claude/CLAUDE.md      # repo-level guidance for Claude Code
  .claude-plugin/        # the Claude Code marketplace plugin
  docs/                  # what you're reading
```

The crate is **one library** (`src/lib.rs`) and **one binary** (`src/main.rs`, 9 lines, wraps `cli::run`). The library exposes every module publicly so integration tests and `serve`'s in-process call sites can use them.

## `src/` module map

```
src/
├── lib.rs                  # module declarations + test isolation ctor
├── main.rs                 # tokio main, calls cli::run
├── onnx_ep.rs              # ONNX execution provider selection (CPU/CoreML/XNNPACK)
│
├── cli/                    # CLI surface
│   ├── mod.rs              # dispatch from parsed Cli into command handlers
│   ├── app.rs              # all Clap structs (Cli, Command, sub-commands)
│   ├── commands/           # one file per subcommand handler
│   │   ├── add.rs, get.rs, query.rs, list.rs, update.rs, delete.rs,
│   │   ├── challenge.rs, review.rs, stats.rs, doctor.rs, gc.rs,
│   │   ├── compress.rs, reindex.rs, migrate.rs, rollback.rs,
│   │   ├── init.rs, serve.rs, daemon.rs, completions.rs,
│   │   ├── setup.rs, hook.rs, projects.rs
│   │   └── mod.rs          # re-exports
│   ├── output.rs           # OutputFormatter: pretty / json / plain rendering
│   ├── prompter.rs         # InquirePrompter for interactive flows
│   └── validation.rs       # shared validation helpers
│
├── mcp/                    # MCP server surface
│   ├── mod.rs
│   ├── server.rs           # EngramDbServer: rmcp #[tool] macros + transports
│   └── error.rs            # MCP-specific error mapping
│
├── ops/                    # the shared operations layer
│   ├── mod.rs              # provider_specs, ProviderCache, embedding_model_report
│   ├── daemon_resolve.rs   # DaemonPolicy, re-resolvable DaemonCell, resolve_providers (CLI+MCP)
│   ├── create.rs           # CreateParams, create_memory, validate_summary
│   ├── query.rs            # query_memories, merge_scored_memories
│   ├── update.rs           # UpdateParams, update_memory
│   ├── delete.rs           # delete_memory
│   ├── get.rs              # get_memory
│   ├── list.rs             # ListParams, list_memories, parse_sort_field
│   ├── challenge.rs        # challenge_memory, challenge_for_contradictions
│   ├── resolve.rs          # ResolveParams, resolve_memory
│   ├── review.rs           # ReviewParams, review_memories
│   ├── gc.rs               # gc_memories
│   ├── compress.rs         # compress_candidates, compress_apply
│   ├── reindex.rs          # reindex
│   ├── stats.rs            # compute_stats
│   ├── doctor.rs           # doctor, doctor_environment (the heavy one)
│   ├── projects.rs         # registry CRUD: list/info/link/unlink/prune/delete
│   └── parsing.rs          # shared enum/value parsers
│
├── storage/                # disk + LanceDB
│   ├── mod.rs              # public re-exports
│   ├── store.rs            # MemoryStore orchestrator (the central type)
│   ├── lance_index.rs      # LanceDB table: schema, IndexEntry, VectorMatch
│   ├── manifest.rs         # manifest.toml: Manifest, EmbeddingFingerprint
│   ├── memory_file/        # frontmatter+markdown serializer
│   ├── paths.rs            # platform paths (project/global/cache/lancedb/...)
│   ├── project_id.rs       # SHA-256-derived 16-char IDs, detect_worktree_main
│   ├── registry.rs         # FileRegistry/InMemoryRegistry, parent-child
│   ├── worktree.rs         # resolve_project_root, consolidate_worktree_into_main
│   ├── write_lock.rs       # per-project advisory flock
│   ├── config.rs           # load_config from config.toml
│   ├── error.rs            # storage::Result, StorageError
│   └── test_support.rs     # cfg(test) helpers
│
├── retrieval/              # query pipeline
│   ├── mod.rs              # re-exports
│   ├── engine.rs           # RetrievalEngine, RetrievalQuery, ScoredMemory (large)
│   ├── filters.rs          # apply_index_filters, SearchFilters
│   └── reranker.rs         # Reranker trait, LocalReranker (BGE/Jina)
│
├── scoring/                # composite scoring
│   ├── mod.rs              # docs of the formula
│   ├── composite.rs        # composite_score, ScoringContext, ScoreBreakdown
│   ├── decay.rs            # decay_factor, effective_relevance
│   └── trust.rs            # trust_weight (Provenance.source → weight)
│
├── scope/                  # physical/logical proximity
│   ├── mod.rs              # scope_proximity helper
│   ├── physical.rs         # file-path + glob proximity with depth decay
│   └── logical.rs          # dot-notation hierarchy bonus
│
├── search/                 # keyword search
│   ├── mod.rs
│   └── keyword.rs          # simple weighted tokenized search over summary/content/tags
│
├── embeddings/             # embedding providers
│   ├── mod.rs              # EmbeddingProvider trait + EmbeddingError
│   ├── onnx.rs             # OnnxProvider via fastembed
│   ├── ollama.rs           # OllamaProvider via reqwest (gated by `ollama` feature)
│   └── chunking.rs         # chunk_text (sentence-boundary splitting with overlap)
│
├── nli/                    # NLI for contradiction detection
│   ├── mod.rs              # NliProvider trait
│   └── onnx.rs             # OnnxNliProvider (tokenizers + ort)
│
├── title/                  # automatic memory title generation
│   ├── mod.rs              # TitleStrategy enum
│   ├── keyword.rs          # keyword-based titles (default)
│   └── t5.rs               # T5-based titles (optional)
│
├── daemon/                 # shared embedding daemon
│   ├── mod.rs              # socket-path resolution
│   ├── server.rs           # daemon event loop
│   ├── client.rs           # DaemonHandle, query_status, request_shutdown
│   ├── protocol.rs         # wire protocol enums + PROTOCOL_VERSION
│   ├── remote.rs           # remote_providers: trait impls calling the daemon
│   ├── metrics.rs          # request metrics persisted to LanceDB
│   └── tests.rs            # daemon integration tests
│
├── telemetry/              # stats + persistence
│   ├── mod.rs              # public API
│   ├── collector.rs        # process-wide stats collector (StatsCollector)
│   └── persistence.rs      # flush to LanceDB; retention; cross-restart cumulatives
│
└── types/                  # data model
    ├── mod.rs
    ├── config.rs           # EngramConfig + every sub-config; defaults
    ├── memory.rs           # Memory, MemoryType, Status, Visibility, MemoryUpdate
    ├── decay.rs            # Decay, DecayStrategy
    ├── provenance.rs       # Provenance, ProvenanceSource
    └── challenge.rs        # Challenge
```

## Where to make changes (by what you want to do)

| You want to… | Edit |
|--------------|------|
| Add a new CLI subcommand | `src/cli/app.rs` (Clap), `src/cli/commands/<new>.rs`, `src/cli/mod.rs` (dispatch), and the `src/ops/<new>.rs` it calls |
| Add a new MCP tool | `src/mcp/server.rs` (the `#[tool]` macro) calling into the same `src/ops/<new>.rs` |
| Change a memory field | `src/types/memory.rs` + `src/storage/lance_index.rs` (schema) + `src/storage/memory_file/` (serializer) + `src/types/config.rs` if it has a default |
| Add a new memory type variant | `src/types/memory.rs::MemoryType` + the default_decay match + `src/ops/parsing.rs::parse_memory_type` |
| Add a new embedding provider | `src/embeddings/<provider>.rs` + extend `provider_specs` in `src/ops/mod.rs` (see [extending.md](./extending.md)) |
| Add a new config field | `src/types/config.rs` (with `#[serde(default)]` + a `default_*` fn) + extend `provider_cache_key` if it affects model loading |
| Change scoring | `src/scoring/composite.rs` (formula) or `src/scoring/decay.rs`, `src/scoring/trust.rs` (the multipliers) |
| Change the retrieval pipeline | `src/retrieval/engine.rs` (stages) or `src/retrieval/filters.rs` (LanceDB filter mapping) |
| Add a hook | `src/cli/commands/hook.rs` + `src/cli/app.rs::HookCommand` + plugin manifest `.claude-plugin/plugin.json` + setup writer `src/cli/commands/setup.rs` |
| Add a daemon RPC | `src/daemon/protocol.rs` (wire) + `src/daemon/server.rs` (handler) + `src/daemon/remote.rs` (client) + bump `PROTOCOL_VERSION` if breaking |
| Touch the LanceDB schema | `src/storage/lance_index.rs` (Arrow schema) + a migration in `src/cli/commands/migrate.rs` |

## Intentionally big files

`src/mcp/server.rs`, `src/cli/app.rs`, `src/retrieval/engine.rs`, `src/ops/doctor.rs`, `src/types/config.rs` are large by design — macros, connected algorithms, or `default_*` schemas that lose cohesion when split. If a split is unavoidable, split along invariants (e.g. config section), not line count.
