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
challenge_penalty = { fact = 0.15, observation = 0.20, decision = 0.05 }
                               # per-epistemic-class subtraction for challenged memories;
                               # a scalar (challenge_penalty = 0.10) is the legacy flat
                               # form, still accepted and applied to every class
depth_decay_base = 0.82        # base for exponential scope-depth decay
depth_decay_floor = 0.3        # minimum scope score regardless of depth
scope_multiplier_floor = 0.5   # neutral base for logical-only scope context

[retrieval.scoring.situation]
floor = 0.6                    # multiplier floor; 1.0 disables situation weighting

[retrieval.scoring.situation.session_start]
fact = 1.0
observation = 0.5
decision = 0.8

[retrieval.scoring.situation.file_edit]
fact = 0.7
observation = 0.7
decision = 1.0

[retrieval.scoring.situation.debugging]
fact = 0.6
observation = 1.0
decision = 0.7

[retrieval.scoring.situation.design_choice]
fact = 0.8
observation = 0.7
decision = 1.0

[search]
semantic_weight = 3.0          # weight on semantic similarity vs keyword
threshold = 0.2                # minimum keyword-search score

[embeddings]
backend = "auto"               # "auto" | "onnx" | "ollama"
provider = "onnx"              # "all-minilm" (384d) | "nomic-embed-text" (768d) | "mxbai-embed-large" (1024d)
dimensions = 384               # must match the provider
max_tokens = 256               # truncate inputs longer than this
reindex_on_model_change = "warn"   # "off" | "warn" | "auto" | "error"
metadata_vector = true         # embed "{title}. {summary}. tags: ..." as an extra vector per memory
                               # (toggling changes vector composition: run `engramdb reindex --embeddings-only`)
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

[review]
recency_days = 90              # active memories not updated in > N days are suggested
                               # for review (recency trigger). Omit the field to keep
                               # the 90-day default; set to a large value to soften it.

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

[security]
allow_cross_project_writes = true   # allow MCP tools to write to a different registered project

[epistemic]
observation_review_days = 90        # observations unverified for longer than this get a
                                    # "re-verify or delete" doctor suggestion
observation_half_life_days = 90     # default decay half-life applied when an
                                    # observation-class memory gets no explicit decay
observation_decay_floor = 0.2       # floor for that default observation decay
demote_on_session_end = false       # SessionEnd hook demotes the session's task-scoped memories
promotion_min_sessions = 3          # distinct later sessions retrieving a task-bound decision
                                    # before promotion is suggested
auto_promote = false                # maintenance auto-promotes instead of suggesting
consolidation_min_sources = 3       # min mutually-consistent observations to merge into a fact
consolidation_similarity = 0.85     # pairwise embedding similarity for consolidation clusters
auto_consolidate = false            # maintenance auto-consolidates instead of suggesting
invalidated_retention_days = 180    # days before gc may purge invalidated memories; 0 = keep forever

[hooks]
prompt_context_budget = 1000        # char budget for UserPromptSubmit / PreToolUse hook injection
# class_order = ["decision", "fact", "observation"]
                                    # optional uniform epistemic-class ordering for hook
                                    # injection; unset = per-situation defaults

[content]
summary_max_chars = 200             # max length (chars) of a memory summary on create/update/resolve

[cli]
project_list_grouping = "auto"      # projects-list layout: auto | always | none
```

## Notes on selected sections

- **`[retrieval.scoring]`** — composite formula: `score = base * scope_multiplier * trust_multiplier * situation_multiplier - challenge_penalty`, clamped to `[0, 1]`. `base` comes from whichever weight set applies (`with_query` / `with_keyword` / `scope_only` / `degraded`). The scope multiplier is the depth-decayed `path` match plus a logical bonus (≤ 0.3) when a `path` is supplied; with only `logical` context it is `scope_multiplier_floor + bonus` for related memories (bare floor for unscoped memories, 0 for unrelated ones); 1.0 (neutral) when no scope context is given. The situation multiplier is 1.0 (neutral) when the query carries no situation.
- **`challenge_penalty`** — per-epistemic-class by default: a contradicted observation is cheap to re-measure (0.20, suppressed hardest), a contradicted fact is probably stale (0.15), a contested decision must remain visible together with its dispute (0.05, mildest). The legacy scalar form (`challenge_penalty = 0.10`) still parses and applies flat to every class. All values must be in `[0, 1]`.
- **`[retrieval.scoring.situation]`** — situation-conditioned class weighting, applied as a post-multiplier when a query (or hook) declares a situation (`session_start`, `file_edit`, `debugging`, `design_choice`): `multiplier = floor + (1 - floor) * profile[situation][class]`. Each profile value is in `[0, 1]`; with the default `floor = 0.6` a class can be down-weighted at most 40%. Set `floor = 1.0` to disable situation weighting entirely. Defaults per situation: session start favors facts, file edits favor decisions, debugging favors observations, design choices favor decisions.
- **`[epistemic]`** — lifecycle knobs for epistemic classes. `observation_review_days` drives a `doctor` suggestion to re-verify (or delete) old unverified observations. `observation_half_life_days` / `observation_decay_floor` set the default decay for observation-class memories that don't specify one. `promotion_min_sessions` + `auto_promote` control when a task-scoped decision retrieved across distinct later sessions is promoted to project-wide (suggest by default, apply when `auto_promote = true`). `consolidation_min_sources` + `consolidation_similarity` + `auto_consolidate` control merging clusters of near-duplicate observations into a derived fact. `invalidated_retention_days` (default **180**, `0` = keep forever) is how long invalidated memories stay on disk before `gc` may purge them; until then they are also exempt from low-score GC. `demote_on_session_end = true` makes the SessionEnd hook demote the session's declared task's memories (same effect as `engramdb task complete`).
- **`[hooks]`** — rendering knobs for the Claude Code hooks. `prompt_context_budget` (default **1000** chars) caps the UserPromptSubmit and PreToolUse injections (SessionStart has its own fixed 2000-char budget). `class_order` optionally replaces the per-situation epistemic-class ordering of injected memories (SessionStart: fact → decision → observation; file edits: decision → fact → observation) with one uniform list.
- **`[content]`** — memory-content constraints. `summary_max_chars` (default **200**) is the maximum length, in characters, of a memory's one-line summary; it is enforced on every `create` / `update` / `resolve --update` path and bounds the auto-generated summary of a `compress`/consolidate merge. Measured in characters (not bytes), so multibyte summaries are not penalized.
- **`[cli]`** — human-readable CLI output preferences (ignored by the MCP surface and every `--json` path). `project_list_grouping` controls how `engramdb projects list` lays out its directory tree: `auto` (default) prints a folder header only for directories holding two or more projects and renders a lone project inline on a full-path line; `always` prints a header above every project (rows show just the basename); `none` prints a flat list of full-path rows with no headers. In all three modes worktree sub-projects nest under their real parent and rows are sorted by path.
- **`[embeddings]`** — changing `provider` or `dimensions` requires `engramdb reindex --embeddings-only`. See [embeddings.md](./embeddings.md) for fingerprinting and the model-change policy.
- **`[trust_weights]`** — `Provenance` source maps to a trust weight (`human` highest, `inferred` lowest). The multiplier is `floor + (1 - floor) * weight`, so even fully `inferred` memories keep ≥50% of their raw score.
- **`[nli]`** — off by default. Downloads ~50 MB and adds latency to `create`. When enabled, every `create` checks the top-`max_comparisons` similar memories and auto-challenges contradictions above `contradiction_threshold`.
- **`[rerank]`** — off by default. Downloads ~100 MB. Final score blends original and reranker: `(1 - weight) * original + weight * rerank_score`.
- **`[review]`** — the recency trigger for reviewing old memories. `recency_days` (default **90**) is the age past which an *active* memory that hasn't been updated (every edit and every `resolve`/keep/update bumps `updated_at`) is suggested for review. It never deletes or hides anything — it only surfaces a suggestion: the MCP `review` tool folds these stale memories in alongside flagged (challenged / needs-review) ones by default, and the MCP `memory-session-end` prompt reports how many are due so an agent can offer to revisit them. Stale memories are ranked by criticality so the ones most worth re-verifying come first. Omit the field to keep the 90-day default; validation accepts 1–3650. On the CLI, `engramdb review --stale-after-days [N]` opts a single run into the trigger (bare flag = 90).
- **`[stats]`** — telemetry events persist to a per-project LanceDB table. `retention_days` defaults to **90** so the event log cannot grow without bound; events older than the window are pruned periodically (by the background flush task and by `engramdb gc`). Set up to the maximum of 3650 (10 years) to effectively retain forever. `0` is rejected by validation — older versions documented it as "retain forever" but actually deleted everything, so an explicit positive value is now required. Lifetime counters in `engramdb stats` cover "since the oldest non-pruned event".
- **`[title]`** — how a memory's title is generated when the caller doesn't supply one. `t5` (default) is abstractive T5-small summarization; the shared daemon / MCP server loads (and pools) the encoder+decoder **once machine-wide**, so the per-`create` cost is amortized. `keyword` is in-process RAKE extraction (no model); `none` skips automatic titling. The one-shot CLI's `engramdb add` always uses `keyword` so a single command never pays a cold T5 load. The MCP `create` tool's per-call `title_strategy` overrides this.
- **`[daemon]`** — see [daemon.md](./daemon.md). `use_for_cli` (default `true`) lets model-needing CLI commands route through a *running* daemon; `--in-process` / `ENGRAMDB_IN_PROCESS` / `use_for_cli = false` force in-process loading.
- **`[security]`** — `allow_cross_project_writes` (default `true`, preserving historical behavior) gates the MCP server's confused-deputy surface. Nearly every MCP tool accepts an optional `project` override that resolves to *any* project in the global registry, so a steered agent operating in project A could otherwise mutate a different registered project B on the same machine. Setting it to `false` blocks the MCP mutating tools (`create`, `update`, `delete`, `challenge`, `resolve`, `compress_apply`, `gc`, `reindex`) from writing to a **different** registered project. The session's own project (`project` omitted) and the shared global store (`project = "global"`) are always allowed; a linked worktree of the session's own project is not treated as cross-project. Read-only tools (`query`, `get`, `list`, `stats`, `review`, `compress_candidates`, `projects_*`, `doctor`) are never gated.

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

