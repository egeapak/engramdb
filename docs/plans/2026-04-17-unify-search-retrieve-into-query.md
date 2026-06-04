# engramdb: Merge `search` + `retrieve` into a single `query` tool

## Context

engramdb today ships two MCP tools (`search` and `retrieve`) that do ~80% the same thing but with inconsistent semantics:

- `retrieve` treats `logical` as a hierarchical **scoring** context (exact/parent/sibling bonuses via `src/scope/logical.rs`) and accepts a multi-element `Vec<String>`.
- `search` treats `logical` as an **exact-match hard filter** on a single `Option<String>` and drops any memory with zero keyword score.

This asymmetry misleads agents (hit in practice: `search(logical="workflow")` returned 0 hits for a memory scoped `workflow.git.pr`, even though engram's own hierarchy code says `workflow` is that memory's parent). The two tools share the same underlying scorer (`composite_score` in `src/scoring/composite.rs`), so the split is an API-shape artifact, not a real capability boundary.

There is also a latent correctness bug in `src/scope/logical.rs::is_parent_scope`: it returns a boolean, not a distance, so a memory scoped `a.b.c.d` compared against context scope `a` currently scores a full parent bonus (+0.2) when it is actually a great-grandchild relationship. After this refactor lands we want graceful decay with depth so the bonus reflects the real proximity.

**Goal:** collapse both tools into a single `query` MCP tool with an explicit `mode` parameter. Treat `logical` as a scoring signal everywhere, not a filter. Replace the boolean hierarchy matcher with a distance-aware one that decays gracefully across ancestor levels so proximity always produces an appropriately scaled signal.

**Intended outcome:**
- One obvious way to ask engram for memories.
- Hierarchical scopes that "just work" — tagging a memory `workflow.git.pr` makes it discoverable (with bonuses that reflect real distance) under `workflow`, `workflow.git`, or a sibling/cousin like `workflow.git.commit`.
- Agent directives in the `ENGRAM.md` template reduce from two framings ("search before answering" + "retrieve before modifying") to one unified directive.

## Design decisions (locked)

1. **`logical` is always a scoring signal, never a filter.** If a user scopes a query with `logical: ["workflow.git.pr"]`, unrelated memories rank lower; they are not excluded.
2. **Single `query` MCP tool. Hard-remove `search` and `retrieve` from MCP.** No deprecation aliases.
3. **CLI: hard-remove `engramdb search` and `engramdb retrieve`. Single `engramdb query` subcommand.** Accepts positional query text *or* `--query <text>`, plus `--mode rank|filter` (required). Consistent with MCP, and avoids maintaining two framings while ENGRAM.md teaches one.
4. **`mode` parameter required, explicit.** Two values:
   - `mode: "rank"` — return everything that passes type/tag/criticality filters, ranked by composite score. The "context-aware browsing" flow.
   - `mode: "filter"` — require at least one positive relevance signal (see validation below). Drop zero-score memories.
5. **Expanded hierarchy decay** — max over all (memory_scope, current_scope) pairs:
   | Relationship | Bonus |
   |---|---|
   | Exact match | +0.30 |
   | Parent ↔ child (distance 1 up/down the tree) | +0.20 |
   | Sibling (same immediate parent) | +0.15 |
   | Grandparent ↔ grandchild (distance 2) | +0.10 |
   | Cousin (same grandparent, not same parent) | +0.05 |
   | Great-grandparent ↔ great-grandchild (distance 3) | +0.05 |
   | Anything deeper or unrelated | 0.00 |

## Files to modify

### Core engine (Rust)

| Path | Change |
|---|---|
| `src/scope/logical.rs` | Replace the boolean `is_parent_scope` + `are_siblings` dispatch in `calculate_scope_bonus` (line 54) with a **distance-aware** function. Add `ancestor_distance(ancestor, descendant) -> Option<usize>` returning segment count iff `ancestor` is a strict ancestor, else `None`. Add `lca_distance(a, b) -> (usize, usize)` returning steps from each scope to their lowest common ancestor (e.g., `a.b.c` vs `a.b.d` → `(1, 1)` = siblings; `a.b.c` vs `a.d.e` → `(2, 2)` = cousins). Map the distances to the new decay table. Keep existing `extract_parent` helper. Add unit tests for grandparent, great-grandparent, cousin, deep no-match, and the specific `a.b.c.d` vs `a` case (must return +0.05, not +0.20). |
| `src/retrieval/engine.rs` | Merge `search()` (line 577) and `retrieve()` (line 370) into a single `query()` method taking `RetrievalQuery` with a new required `mode: RetrievalMode` field. Remove the `logical[0]` → filter reduction at line 379. Move the `raw_kw == 0 → skip` gate (line 632) behind `mode == Filter` and generalize it to the sufficiency check (see below). Rename the output field `query_mode` → `retrieval_quality` on `RetrievalResult` to avoid name collision with the input `mode` param. Values stay the same strings (`"full"`/`"keyword_only"`/`"no_query_signals"`/`"scope_only"`), just a cleaner label. |
| `src/retrieval/filters.rs` | Drop `SearchFilters.logical` field entirely (scope is no longer a filter). Remove the corresponding check in `apply_index_filters` (lines 131-136). Keep `physical` as a filter (file-path globs are naturally exclusionary; hierarchy bonuses don't apply). Update the test at `test_filter_by_logical_scope` (lines 289-325) — either delete it or repurpose for physical scope assertions. |
| `src/ops/search.rs` + `src/ops/retrieve.rs` | Delete both. Create `src/ops/query.rs` exposing `pub async fn query_memories(engine: &RetrievalEngine, query: &RetrievalQuery) -> Result<RetrievalResult>`. Update `src/ops/mod.rs` re-exports (remove `pub use retrieve::retrieve_memories;` at mod.rs:45 and `pub use search::search_memories;` at mod.rs:47; add `pub use query::query_memories;`). |

#### `RetrievalMode` enum

Add alongside `RetrievalQuery` in `src/retrieval/engine.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalMode {
    /// Return everything passing type/tag/criticality filters, ranked by composite score.
    Rank,
    /// Require at least one positive relevance signal. Drop zero-score memories.
    Filter,
}
```

#### Filter-mode sufficiency check

Runs after scoring, before threshold-drop. A memory is "sufficient" in `Filter` mode when **any** of the following is true:

- Keyword score (raw `keyword_search` result) > 0
- Semantic score (embedding similarity) > 0
- Scope proximity (`logical` or `path`) > 0
- Tag filter (`filters.tags`) provided and matched against this memory

`min_criticality` alone is **not** sufficient — it's an importance label, not a relevance signal. Memories passing only `min_criticality` in Filter mode get dropped.

#### Filter-mode validation error

At the entrypoint to `query()` (before `apply_index_filters`), when `mode == Filter`, require at least one of `query`, `logical`, `path`, `tags` to be non-empty. If all four are empty, return `ValidationError("mode=filter requires at least one of: query, logical, path, tags")`. Shared between CLI and MCP call sites.

### MCP server

| Path | Change |
|---|---|
| `src/mcp/server.rs` | Delete `SearchInput` (line 131), `RetrieveInput` (line 92), `memory_search` handler (line 873), `memory_retrieve` handler (line 780). Add `QueryInput` struct and `memory_query` handler (see shape below). Update inline tests (lines 2276-2505) — the `search_*` and `retrieve_*` tests fold into `query_*` variants covering both modes. |

`QueryInput` shape:

```rust
#[derive(Debug, Deserialize, JsonSchema)]
struct QueryInput {
    #[schemars(description = "Mode: \"rank\" for context-aware ranked results, \"filter\" for specific-term search (drops zero-signal matches). Required.")]
    mode: String,

    #[schemars(description = "Search query text (tokenized against summary, content, tags)")]
    query: Option<String>,

    #[schemars(description = "Physical scope — current file path for scope-proximity scoring")]
    path: Option<String>,

    #[schemars(description = "Logical scopes in dot notation — contributes to hierarchy-proximity score (not a filter)")]
    logical: Option<Vec<String>>,

    #[schemars(description = "Filter by memory types")]
    types: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Minimum criticality threshold")]
    min_criticality: Option<f64>,

    #[schemars(description = "Maximum results (default 10)")]
    max_results: Option<usize>,

    #[schemars(description = "Detail: summary|content|full (default content) — available in both modes")]
    detail_level: Option<String>,

    #[schemars(description = "Include expired/decayed memories")]
    include_expired: Option<bool>,

    #[schemars(description = "Also include global memories in results (default false)")]
    include_global: Option<bool>,

    #[schemars(description = "Target project: absolute path, 16-char project ID, or \"global\". Omit for current project.")]
    project: Option<String>,
}
```

Tool description (agents read this to decide when to call):

> Query all memories. Use `mode: "rank"` to get memories ranked by relevance to a context (current file path, topic, logical scope) — good before modifying files or when orienting. Use `mode: "filter"` to find memories containing specific terms, scopes, or tag matches — good when you have a concrete lookup. Filter mode requires at least one of `query`, `logical`, `path`, or `tags`.

Validation:
- `mode` must be exactly `"rank"` or `"filter"` (anything else → `ValidationError`). Done in the MCP handler before delegating.
- Empty-filter check handled in the engine entrypoint (shared with CLI).

### CLI

| Path | Change |
|---|---|
| `src/cli/app.rs` | Replace the `Retrieve { ... }` variant (line 220) and `Search { ... }` variant (line 263) with a single `Query { mode, query, path, logical, types, tags, min_criticality, max_results, detail_level, include_expired }` variant. `mode` is a required Clap arg (`#[arg(long, value_enum)] mode: RetrievalMode`). `query` accepts either a positional arg or `--query <text>`. Update all matchers at lines 582, 633, 791, 802, 822, 834, 847, 862, 874, 893, 907, 929, 941. |
| `src/cli/mod.rs` | Update dispatch (lines 150-181). |
| `src/cli/commands/search.rs` + `src/cli/commands/retrieve.rs` | Delete both. Add `src/cli/commands/query.rs`. |
| `tests/cli/search.rs` + `tests/cli/retrieve.rs` | Delete both. Add `tests/cli/query.rs` covering rank mode (with path, with logical, with both), filter mode (with query, with logical alone, with tag alone), filter-mode validation error (no positive signal), and the `min_criticality`-alone-in-filter-mode rejection. |

User-facing invocation: `engramdb query --mode filter "text"` / `engramdb query --mode rank --path src/foo.rs`.

### Hooks

| Path | Change |
|---|---|
| `src/cli/commands/hook.rs` | Swap `retrieve_memories` → `query_memories` at lines 208 and 288, hardcoded `mode: RetrievalMode::Rank` (both hooks are context-browsing flows — rank is correct). **Also update the user-visible format string at line 109** from "use search to find them" to "use query to find them" — otherwise the surfaced hint references a deleted tool. The test at line 622 must also be updated. |

### Docs / agent directives

| Path | Change |
|---|---|
| `src/cli/commands/setup.rs:9` (`ENGRAM_MD_CONTENT`) | Collapse the "Search before answering" and "Retrieve before modifying" bullets into one: *"Query before answering or modifying — call `query` with `mode: \"rank\"` to surface memories relevant to your current file, topic, or logical scope; call with `mode: \"filter\"` (plus a `query` text, `logical` scopes, `path`, or `tags`) to find specific memories."* |
| `src/cli/commands/setup.rs:22-40` (`MCP_TOOL_SUFFIXES`) | Replace `"search"` and `"retrieve"` entries with `"query"`. Count drops from 16 to 15. |
| `src/cli/commands/setup.rs` (`run_setup`) | **Add a cleanup pass**: before writing/updating the MCP permission entries, detect and remove stale entries for `mcp__plugin_engram_memory__search`, `mcp__plugin_engram_memory__retrieve`, `mcp__engramdb__search`, `mcp__engramdb__retrieve` from `permissions.allow` in the target `settings.json`. Without this, existing installations hit a permission prompt every time the new `query` tool is called (stale entries in the allowlist don't match the new tool name). The file-writing machinery already exists around lines 140-200 (the exact area where the permission list is appended) — add a removal step before the append. |
| `README.md` (lines 94-95, 122-123) | Replace the `search` and `retrieve` rows with a single `query` row in both tool tables. |
| `.claude-plugin/README.md` (lines 43, 58-59) | Update the "16 tools" → "15 tools" and the `search`/`retrieve` usage bullets to a single `query` bullet mirroring the ENGRAM.md directive. |

### Benchmarks

| Path | Change |
|---|---|
| `benches/benchmarks.rs`, `benches/helpers.rs` | 9 references total. Update to `query_memories` / `engine.query()` with explicit mode so perf baselines stay valid. Rank-mode benches mirror old retrieve benches; add at least one filter-mode bench to capture the sufficiency-check overhead. |

## Reuse (no new code where existing code fits)

- Scoring: `src/scoring/composite.rs::composite_score` already handles the unified path via `ScoringContext::{with_keyword, with_query_degraded, with_semantic, scope_only}`. The new `query()` picks the right constructor based on whether a query string and embeddings are present — same branching as today's `retrieve()` at engine.rs:464-495.
- Hierarchy: `src/scope/logical.rs::extract_parent` already exists. New `ancestor_distance` and `lca_distance` compose it.
- Filtering: `src/retrieval/filters.rs::apply_index_filters` stays, minus the `logical` branch.
- Reranking: `src/retrieval/engine.rs` rerank step (lines 523-527) is unchanged — fires only when `query.query.is_some()`, works identically in both modes.
- `include_global` merge: `merge_scored_memories` in `src/mcp/server.rs` already handles both paths — one call site after the merge (in `memory_query`).

## Verification

### Unit tests
- `cargo test -p engramdb scope::logical` — new decay matrix (grandparent +0.10, great-grandparent +0.05, cousin +0.05, deep no-match). **Must include the `a.b.c.d` vs `a` = +0.05 case** (today returns +0.20 due to the boolean-matcher bug).
- `cargo test -p engramdb retrieval::engine` — mode branching, filter-mode validation error, sufficiency check (keyword/semantic/scope/tag sufficient; criticality-alone insufficient), `logical` no longer dropping results.
- `cargo test -p engramdb scoring::composite` — unchanged expectations; this is a regression guard.
- `cargo test -p engramdb mcp::server` — new `query_*` tests replacing the old `search_*` / `retrieve_*` set, plus one asserting `mode` is required.
- `cargo test --test cli` — CLI `query` command in both modes plus the filter-mode validation error path.

### Integration smoke test
From `/Users/egeapak/Projects/ceiba/EClinicsHospitalService`:
1. Build + install locally: `cargo install --path /Users/egeapak/Projects/personal/engramdb --force`
2. Refresh permissions + strip stale entries: `engramdb setup --global`
3. Confirm no stale `mcp__plugin_engram_memory__search` or `_retrieve` entries remain in `~/.claude/settings.json`.
4. In a fresh Claude Code session, invoke the MCP `query` tool with:
   - `mode: "filter", query: "PR creation"` → Azure DevOps PR memory returns at rank 1 (today's baseline).
   - `mode: "rank", logical: ["workflow.git"]` → same memory (scoped `workflow.git.pr`) with a **parent** bonus (+0.20) visible in `score_breakdown.scope`.
   - `mode: "rank", logical: ["workflow"]` → same memory with a **grandparent** bonus (+0.10) — previously would have scored +0.20 due to the boolean bug; after fix, reflects true distance.
   - `mode: "rank", logical: ["workflow.docs.pr"]` → **cousin** bonus (+0.05) — neither parent nor sibling, but shares grandparent `workflow`.
   - `mode: "filter"` with no `query`/`logical`/`path`/`tags` → `ValidationError: "mode=filter requires at least one of: query, logical, path, tags"`.
   - `mode: "filter", min_criticality: 0.8` (no other signal) → same `ValidationError` (criticality alone insufficient).

### Regression check
- Run the existing MCP tests on `main` to capture baseline output, then on the refactor branch, diff the score breakdowns. Expected: `keyword` scores unchanged; `scope` scores **decrease** for deep ancestor relationships that previously scored a full +0.20 parent bonus despite being grandparents or further (this is the intended correctness fix, not a regression). Same-level relationships (exact/parent/sibling) stay identical.
- Simulate a `SessionStart` hook invocation using existing test patterns in `src/cli/commands/hook.rs` tests — confirm rank mode still surfaces the same memories and the "use query to find them" string appears.

## Out of scope (explicit)

- `physical` scope remains a hard filter (file-path globs are naturally exclusionary).
- No new score components beyond the hierarchy decay expansion.
- No migration tool for existing memories — stored memory shape is unchanged.
- No deprecation shims, no hidden aliases. Clean cut, single release.
- No changes to embedding/rerank pipelines.

## Rough effort & shipping order

~2-3 focused days, shipped in this order to minimize risk:

1. **Hierarchy decay expansion + tests** — ~0.5 day. Isolated to `src/scope/logical.rs` and its tests. Orthogonal to the MCP refactor; validates the correctness fix before it gets entangled with the bigger change. Land first.
2. **Core engine `query()` merge + filter cleanup** — ~1 day. `src/retrieval/engine.rs`, `src/retrieval/filters.rs`, `src/ops/*`. Internal API change; no user-facing surface moves yet.
3. **MCP tool merge + permission cleanup** — ~0.5 day. `src/mcp/server.rs`, `src/cli/commands/setup.rs`. User-visible breaking change lands here.
4. **CLI + hooks + docs** — ~0.5 day. `src/cli/app.rs`, `src/cli/mod.rs`, `src/cli/commands/query.rs`, `src/cli/commands/hook.rs`, `README.md`, `.claude-plugin/README.md`, the hook format string at hook.rs:109.
5. **Benchmarks + final verification** — ~0.25 day.
