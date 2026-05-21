# Configuration

EngramDB reads per-project configuration from `<project>/.engramdb/config.toml`. The file is created (empty) by `engramdb init` and every field has a default — you only need to set what you want to change.

Some settings (paths, embedding backend, daemon socket) also accept environment-variable overrides; those have precedence over the config file, and CLI flags have precedence over both.

## File location and layering

| Layer | Source |
|-------|--------|
| Built-in defaults | hard-coded in `src/types/config.rs` |
| `config.toml` | `<project>/.engramdb/config.toml` |
| Environment | `ENGRAMDB_DAEMON_SOCKET`, `ENGRAMDB_EMBEDDING_BACKEND`, `ENGRAMDB_DATA_DIR`, `ENGRAMDB_CONFIG_DIR` |
| CLI flag | `--embedding-backend`, `--socket`, etc. |

Higher rows lose to lower rows. There is no global `config.toml` — each project gets its own.

## Full schema

The example below shows **every** field with its default. Every section is optional, and every field within a section is optional — you can write a `config.toml` that contains only the few lines you actually want to override.

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

[search]
semantic_weight = 3.0          # weight on semantic similarity vs keyword
threshold = 0.2                # minimum keyword-search score

[embeddings]
backend = "auto"               # "auto" | "onnx" | "ollama"
provider = "onnx"              # "all-minilm" (384d) | "nomic-embed-text" (768d) | "mxbai-embed-large" (1024d)
dimensions = 384               # must match the provider
max_tokens = 256               # truncate inputs longer than this
reindex_on_model_change = "warn"   # "off" | "warn" | "auto" | "error"

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
retention_days = 0             # 0 / unset = retain forever (max 3650)
flush_interval_secs = 60
followup_window_secs = 60
max_sessions_per_project = 10000

[daemon]
enabled = true
idle_timeout_secs = 900        # 15 min idle → daemon exits
# socket_path = "/run/user/1000/engramdb/daemon.sock"   # optional override
```

## What the sections do

### `[retrieval]` and `[retrieval.scoring]`

Controls what `engramdb query` returns. The scoring sub-sections define the weight blend used depending on what kind of query came in:

- **`with_query`** — there's a query string **and** scope context. Blends semantic similarity and relevance (criticality × decay).
- **`with_keyword`** — keyword search path. Blends keyword, semantic, and relevance.
- **`scope_only`** — no query, just a scope context. Relevance dominates.
- **`degraded`** — embeddings unavailable. Falls back to relevance-only.

The full composite formula is:

```
score = base * scope_multiplier * trust_multiplier - challenge_penalty
```

where `base` comes from one of the four weight sets above, `scope_multiplier` is derived from `depth_decay_*`, and `trust_multiplier` is bounded by `trust_multiplier_floor`. The result is clamped to `[0, 1]`.

### `[search]`

Keyword search uses a simple weighted scoring over summary, content, and tags. `semantic_weight` is how much extra weight to give a semantic match on top of a keyword match. `threshold` filters out low-score results before they hit the composite scorer.

### `[embeddings]`

| Field | Notes |
|-------|-------|
| `backend` | `auto` tries ONNX first, falls back to Ollama; `onnx` forces local; `ollama` requires a running Ollama instance. |
| `provider` | Model name. Must be one of the three table entries. Changing this requires `engramdb reindex --embeddings-only`. |
| `dimensions` | Must match the provider's actual dimensionality. Mismatch is caught at startup. |
| `max_tokens` | Inputs longer than this are silently truncated by the model. EngramDB chunks long content automatically before embedding. |
| `reindex_on_model_change` | What happens when the manifest's stored fingerprint doesn't match the live provider: `off` (silent), `warn` (default — surfaces a warning), `auto` (auto-reindex), `error` (refuse to serve until reindexed). |

See [embeddings.md](./embeddings.md) for details on model swaps and reindexing.

### `[scope_proximity]` and `[logical_bonus]`

Tune how much weight different kinds of scope matches contribute. Physical scope (file paths) returns `[exact_file, project_root]`; logical bonus stacks on top, capped so the combined scope score never exceeds 1.0.

### `[trust_weights]` and `[retrieval.scoring].trust_multiplier_floor`

`Provenance` source maps to a trust weight (`human` highest, `inferred` lowest). The trust multiplier is `floor + (1 - floor) * weight`, so even a fully `inferred` memory keeps at least 50% of its raw score by default.

### `[thresholds]`

Drive lifecycle ops:
- `needs_review` — composite score below this flags a memory for `engramdb review`.
- `gc` — default threshold for `engramdb gc`.
- `compress` — criticality threshold for `engramdb compress` candidates.

### `[nli]` — contradiction detection

When enabled, every `create` runs the new memory through an ONNX NLI model against the top-`max_comparisons` semantically-similar existing memories. Contradictions above `contradiction_threshold` auto-challenge the conflicting memory.

Off by default because it downloads a separate model (~50 MB) and adds latency to `create`.

### `[rerank]` — cross-encoder reranking

When enabled, the top `top_n` results of every query are re-ranked with a cross-encoder (BGE family by default). The final score is a blend: `(1 - weight) * original + weight * rerank_score`.

Off by default because the reranker model is ~100 MB and adds latency.

### `[stats]` — telemetry

EngramDB records per-stage latencies, query counts, and session telemetry. It's persisted to the global LanceDB store so `engramdb stats --all-projects` and `engramdb stats --daemon` work across restarts.

`retention_days = 0` means no automatic pruning. `enabled = false` disables the whole thing.

### `[daemon]` — shared embedding daemon

| Field | Notes |
|-------|-------|
| `enabled` | Default true. When true, the MCP server delegates inference to a shared daemon (auto-spawned on demand). When false, every MCP process loads its own copy of the models. |
| `idle_timeout_secs` | Daemon exits after this long with no active connections. Default 15 min. |
| `socket_path` | Override the Unix socket path. Resolution chain: `--socket` flag > `ENGRAMDB_DAEMON_SOCKET` env > this field > default per-user runtime path. |

See [daemon.md](./daemon.md).

## Environment variables

| Variable | Effect |
|----------|--------|
| `ENGRAMDB_DAEMON_SOCKET` | Override daemon socket path. |
| `ENGRAMDB_EMBEDDING_BACKEND` | Override `[embeddings].backend` (`auto`/`onnx`/`ollama`). CLI flag wins. |
| `ENGRAMDB_DATA_DIR` | Override platform global data dir. Used by tests for isolation. |
| `ENGRAMDB_CONFIG_DIR` | Override platform global config dir. Used by tests. |
| `RUST_LOG` | Standard `tracing` filter (e.g. `RUST_LOG=engramdb=debug`). |

## Tips

- **Don't commit `personal`-visibility memories.** They live in the global data dir, not in `.engramdb/memories/`, so they don't show up in git anyway. The `[visibility]` field on a memory is what flips this.
- **Don't change `dimensions` without `reindex`.** The manifest records the embedding fingerprint and engramdb will refuse-or-warn (per `reindex_on_model_change`) until vectors match.
- **`reindex_on_model_change = "auto"`** is the safest setting if multiple agents share a store — but it can trigger an expensive re-embed on the next session, which may be surprising. `warn` is the default for that reason.
