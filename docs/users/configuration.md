# Configuration

EngramDB reads per-project config from `<project>/.engramdb/config.toml`. Each section is optional — omit any section to take its defaults. But a section you *do* write must be complete: once a section header appears, every field in it that has no built-in default becomes required (TOML deserialization rejects the section otherwise). The fields without defaults are `[embeddings]`'s `provider` / `dimensions` / `max_tokens`, the three `[retrieval.scoring]` weight sub-tables (`with_query`, `scope_only`, `degraded`) and their `relevance` weights, all of `[thresholds]` (`needs_review` / `gc` / `compress`), all of `[search]`, `[scope_proximity]`, `[logical_bonus]`, `[trust_weights]`, `[nli]`, and `[rerank]`. The full schema below lists every field, so copying the section you want to change (in full) is the safe way to override. There is no global `config.toml`; each project gets its own.

## Override precedence

| Layer | Source |
|-------|--------|
| Built-in defaults | `crates/engram-types/src/config.rs` |
| `config.toml` | `<project>/.engramdb/config.toml` |
| Environment | `ENGRAMDB_DAEMON_SOCKET`, `ENGRAMDB_EMBEDDING_BACKEND`, `ENGRAMDB_DATA_DIR`, `ENGRAMDB_CONFIG_DIR` |
| CLI flag | `--embedding-backend`, `--socket`, etc. |

Higher rows lose to lower rows.

## Full schema

Every field with its default:

```toml
[retrieval]
relevance_threshold = 0.45     # minimum composite score to return
max_results = 10
include_expired = false        # set true to surface decayed memories

[retrieval.scoring.with_query]
semantic = 0.55
relevance = 0.45

[retrieval.scoring.with_keyword]
keyword = 0.45
semantic = 0.30
relevance = 0.25

[retrieval.scoring.scope_only]
relevance = 1.0                # semantic = none

[retrieval.scoring.degraded]
relevance = 1.0                # fallback when embeddings unavailable

[retrieval.scoring]
trust_multiplier_floor = 0.5   # ceiling on how much low-trust memories are suppressed
challenge_penalty = 0.10       # flat subtraction for challenged memories
depth_decay_base = 0.82        # base for exponential scope-depth decay
depth_decay_floor = 0.3        # minimum scope score regardless of depth
scope_multiplier_floor = 0.5   # neutral base for logical-only scope context

[search]
semantic_weight = 3.0          # weight on semantic similarity vs keyword
threshold = 0.2                # minimum keyword-search score

[embeddings]
backend = "auto"               # "auto" | "onnx" | "ollama"
provider = "onnx"              # "all-minilm" (384d) | "nomic-embed-text" (768d) | "mxbai-embed-large" (1024d)
dimensions = 384               # must match the provider
max_tokens = 256               # truncate inputs longer than this
reindex_on_model_change = "warn"   # "off" | "warn" | "auto" | "error"
# pool_size = 2                # independent embedding sessions; omit to auto-size (cores/2) in daemon/MCP, 1 in one-shot CLI

[scope_proximity]
exact_file = 1.0
same_directory = 0.85
same_module = 0.6
project_root = 0.4

[logical_bonus]
exact = 0.3
parent = 0.2
sibling = 0.15

[trust_weights]
human = 1.0
agent = 0.85
inferred = 0.6
imported = 0.7

[thresholds]
needs_review = 0.3             # composite score below this → mark needs-review
gc = 0.05                      # score below this → eligible for gc
compress = 0.4                 # criticality below this → eligible for compression

[nli]
enabled = false
model = "Xenova/nli-deberta-v3-xsmall"
contradiction_threshold = 0.7
max_comparisons = 10
similarity_threshold = 0.3

[rerank]
enabled = false
model = "bge-reranker-base"    # bge-reranker-base | bge-reranker-v2-m3 | jina-reranker-v1-turbo-en | jina-reranker-v2-base-multilingual
top_n = 50
weight = 0.5                   # 0.0 = ignore reranker, 1.0 = trust it fully

[stats]
enabled = true
histogram_capacity = 256
retention_days = 90            # prune events older than N days (default 90, max 3650)
flush_interval_secs = 60
followup_window_secs = 60
max_sessions_per_project = 10000

[daemon]
enabled = true
idle_timeout_secs = 900        # 15 min idle → daemon exits
use_for_cli = true             # route model-needing CLI commands through a running daemon
# socket_path = "/run/user/1000/engramdb/daemon.sock"   # optional override

[title]
strategy = "t5"                # "t5" (default) | "keyword" | "none"
# pool_size = 2                # T5 sessions to pool when strategy = "t5"; omit to auto-size (2, clamped to cores)
```

## Notes on selected sections

- **`[retrieval.scoring]`** — composite formula: `score = base * scope_multiplier * trust_multiplier - challenge_penalty`, clamped to `[0, 1]`. `base` comes from whichever weight set applies (`with_query` / `with_keyword` / `scope_only` / `degraded`). The scope multiplier is the depth-decayed `path` match plus a logical bonus (≤ 0.3) when a `path` is supplied; with only `logical` context it is `scope_multiplier_floor + bonus` for related memories (bare floor for unscoped memories, 0 for unrelated ones); 1.0 (neutral) when no scope context is given.
- **`[embeddings]`** — changing `provider` or `dimensions` requires `engramdb reindex --embeddings-only`. See [embeddings.md](./embeddings.md) for fingerprinting and the model-change policy.
- **`[trust_weights]`** — `Provenance` source maps to a trust weight (`human` highest, `inferred` lowest). The multiplier is `floor + (1 - floor) * weight`, so even fully `inferred` memories keep ≥50% of their raw score.
- **`[nli]`** — off by default. Downloads ~50 MB and adds latency to `create`. When enabled, every `create` checks the top-`max_comparisons` similar memories and auto-challenges contradictions above `contradiction_threshold`.
- **`[rerank]`** — off by default. Downloads ~100 MB. Final score blends original and reranker: `(1 - weight) * original + weight * rerank_score`.
- **`[stats]`** — telemetry events persist to a per-project LanceDB table. `retention_days` defaults to **90** so the event log cannot grow without bound; events older than the window are pruned periodically (by the background flush task and by `engramdb gc`). Set up to the maximum of 3650 (10 years) to effectively retain forever. `0` is rejected by validation — older versions documented it as "retain forever" but actually deleted everything, so an explicit positive value is now required. Lifetime counters in `engramdb stats` cover "since the oldest non-pruned event".
- **`[title]`** — how a memory's title is generated when the caller doesn't supply one. `t5` (default) is abstractive T5-small summarization; the shared daemon / MCP server loads (and pools) the encoder+decoder **once machine-wide**, so the per-`create` cost is amortized. `keyword` is in-process RAKE extraction (no model); `none` skips automatic titling. The one-shot CLI's `engramdb add` always uses `keyword` so a single command never pays a cold T5 load. The MCP `create` tool's per-call `title_strategy` overrides this.
- **`[daemon]`** — see [daemon.md](./daemon.md). `use_for_cli` (default `true`) lets model-needing CLI commands route through a *running* daemon; `--in-process` / `ENGRAMDB_IN_PROCESS` / `use_for_cli = false` force in-process loading.

## Environment variables

| Variable | Effect |
|----------|--------|
| `ENGRAMDB_DAEMON_SOCKET` | Override daemon socket path. |
| `ENGRAMDB_EMBEDDING_BACKEND` | Override `[embeddings].backend` (`auto`/`onnx`/`ollama`). CLI flag wins. |
| `ENGRAMDB_IN_PROCESS` | Truthy (`1`/`true`/`yes`/`on`) forces CLI model loading in-process instead of via the daemon. The `--in-process` flag wins. |
| `ENGRAMDB_DATA_DIR` | Override platform global data dir. Used by tests for isolation. |
| `ENGRAMDB_CONFIG_DIR` | Override platform global config dir. Used by tests. |
| `ENGRAMDB_MODEL_CACHE_DIR` | Override the model-download cache dir (used verbatim). Separate from the data dir. |
| `ENGRAMDB_OFFLINE` | Truthy makes the embedding/NLI/T5 loaders refuse to download uncached models (fail fast instead). |
| `RUST_LOG` | Standard `tracing` filter (e.g. `RUST_LOG=engramdb=debug`). |

