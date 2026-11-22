# Performance conventions

Rules for every change to a hot path in EngramDB. Coding agents must follow
them strictly. Each rule comes from a measurement in one of these documents:

- [allocation-audit.md](./allocation-audit.md): allocations, heap, resident
  memory (RSS, resident set size) and CPU on the hot paths.
- [query-latency-profile.md](./query-latency-profile.md): where a query
  spends its time.
- [parallelization-simd.md](./parallelization-simd.md): threads, SIMD
  (single instruction, multiple data) and the build profile.
- [dashboard-performance.md](./dashboard-performance.md): the dashboard's
  budgets and benches.
- [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md): the daemon, model and hook
  rules.

This page holds the rules. The documents above hold the numbers and the
history.

## Hot paths

A change is on a hot path when it touches one of these:

| hot path | where | why it is hot |
|---|---|---|
| Hooks | `crates/engram-cli/src/commands/hook.rs`, `src/daemon/hooks.rs` | PreToolUse and PostToolUse run on every tool call. UserPromptSubmit runs on every prompt. |
| Query and retrieval | `src/retrieval/`, `src/scoring/`, `src/scope/` | Every `query`, every hook that injects context. |
| Index reads | `crates/engram-storage/src/lance_index.rs`, `store/list.rs` (`list_for_filtering*`, `get_batch`), `store/staleness.rs` (`check_staleness`) | Every query and every hook reads the index. |
| Memory-file parse and write | `crates/engram-storage/src/memory_file/` | Every `get_batch`, `create`, `update` and `reindex`. |
| Daemon request path | `src/daemon/server.rs`, `src/ops/provider_cache.rs`, `crates/engram-models/` | One process serves every session on the machine for days. |
| Transcript digest and defang | `crates/engram-storage/src/transcripts.rs`, `src/ops/harvest.rs` | Runs over transcripts of 1–3 MB each. |
| Dashboard layout and graph | `crates/engram-dashboard/src/layout/`, `crates/engram-dashboard/web/` | Has fixed budgets per store size. |

## Rule zero: measure each change alone, and keep the output the same

- **When you change a hot path for performance, measure that one change
  alone.** Compare it with the binary just before it, in one interleaved
  run. Keep it only when the gain is larger than the noise.
  Why: the audit measured 12 changes one by one. Two looked good and were
  slower or not measurable (see [Do not retry](#do-not-retry)).
- **A performance change must not change output.** Ranking, injected text,
  file bytes and layouts stay identical. The pins in
  [Output pins](#output-pins) must pass with no regeneration.
  If a change must alter output, it is not a performance change. Make it a
  separate change with its own review.
- **Record what you measured.** When you keep or reject a change, add its
  numbers to the matching document above. A rejected idea goes under
  "Rejected" so nobody tries it again.

## Index reads (LanceDB)

### 1. Project columns with `project_columns`, never with `Select::Columns`

Guard: `lance_index::tests::no_library_code_projects_with_select_columns`
fails on any `Select::Columns` in library code (`src/` and `crates/*/src/`).

Why: `Select::Columns` quotes each name. A quoted name goes through Lance's
full SQL parser and a schema conversion per column, on every read. This was
about 45% of a path query's allocations. The fix (c1) removed 55% of the
allocations of a 100-memory text query and 63% of a path query.

```rust
// Don't
table.query().select(lancedb::query::Select::Columns(vec!["id".into()]))

// Do (crates/engram-storage/src/lance_index.rs)
table.query().select(crate::lance_index::project_columns(vec!["id".into()]))
```

`project_columns` accepts plain top-level column names only. It does not
accept dotted names or expressions.

### 2. Put every narrowing into the LanceDB query, never after it

Why: a filter after a k-nearest-neighbour `limit` can return nothing when
`limit` nearer rows do not match. The caller then reports an absence it
cannot prove. A prefilter also reads fewer rows.

```rust
// Don't: search with `limit`, then drop rows outside `since` in Rust.

// Do (crates/engram-storage/src/conversation_index.rs): resolve `since` to a
// session-id set, then push it into the query. Emit the shorter list.
Window::Only(format!("session_id IN ({})", quoted_list(&inside)))
Window::Only(format!("session_id NOT IN ({})", quoted_list(&outside)))
```

### 3. Read only the columns and rows the caller needs

Why: a projection costs per column per row (query-latency-profile R3). At
10,000 memories, leaving out `keyword_stems` on a query without text took
the scope-only Rank from 61 ms to 37 ms (R7).

- When the query has no text, read with `FilterProjection::NoStems`.
- When the caller needs only memories with `watch_paths`, call
  `list_watched`, not `list_for_filtering`.
- When a hook already showed some ids, pass them in
  `RetrievalQuery::skip_load`, so the engine does not load them.

### 4. Make one batched read, not one store call per item

Why: each call opens the table, reads the manifest and plans a query. In a
loop this is O(n²). `chunks_for_memory` in a loop took 13.5 s for 256
memories. `chunks_for_memories` took 136 ms (98.7×).

```rust
// Don't
for id in &ids {
    let v = index.chunks_for_memory(id).await?;
}

// Do (crates/engram-storage/src/lance_index.rs)
let by_id = index.chunks_for_memories(&id_refs).await?;
```

Other batched calls: `MemoryStore::get_batch` (files),
`MemoryStore::edges_touching_any` (edges), `delete_chunks_batch`. When an
`IN (…)` list can be long, split it into chunks (500 ids for chunks and
edges, `MAX_IN_LIST` = 512 for usage events). A scalar index does not fix a
per-item loop. It removes only the row filter, not the open and the plan.

### 5. Do not add LanceDB writes to read paths or hook paths

Why: fragment count, not row count, sets query cost. At a fixed 400 rows,
100 small writes made `list_for_filtering` 7.1× slower and a full query 5×
slower (query-latency-profile R2).

- When a write would change nothing, commit nothing. Example: the edge
  write path reads `owns_any` first, because Lance creates a new version
  even for a delete that matches no row.
- When a hook records an event, append a JSON line through `state_file` and
  let `drain_to_table` move it into the table later. Do not write to
  LanceDB once per hook.
- Compaction runs from the read path (`ops::maintenance::auto_compact`).
  Its common case costs one `stat`. Do not move it to the `create` path:
  that was tried and it added latency to the write and reordered the
  background ingest.

### 6. Keep the default staleness check cheap

Why: `MemoryStore::check_staleness` runs on every `query`, `list` and
`get`. `[index].staleness_check` has three tiers (`counts`, `size`,
`content`), and the default is `size`. When you add a check to it, add it
to the tier that pays for it. Do not make the default tier read or hash
files. `doctor`, `stats` and `reindex --dry-run` are the places that hash
every file.

## Query and retrieval

### 7. Score from the index row; read `.md` files only for survivors

Why: a text query used to load the file of every candidate. At 10,000
memories that was 183 ms of a 233 ms query. Scoring from the row took the
query to 60 ms (query-latency-profile R7).

```rust
// Don't: call store.get_batch(&all_candidate_ids) before scoring.

// Do (src/retrieval/engine.rs): score each row with `score_target`, cut to
// `max_results`, then read only the survivors and their expanded
// neighbours with `load_missing`.
```

Files are read only for rows without stored stems, for the reranker's
`top_n`, and for the results after the cut.

### 8. A knob that is off costs nothing

Why: graph and usage signals are off by default. With every knob off, the
engine does no edge or usage lookup at all, and the ranking is bit-exact
with the ranking before the knobs existed.

- When you add a scoring signal, attach it only when its knob is on.
- Keep `default_config_reads_no_edges_or_usage` and
  `graph_signal_tests::default_config_ranking_is_unchanged_on_graph_fixture`
  passing (`src/retrieval/engine.rs`).

### 9. Cache per process, not per engine

Why: every front end builds a new `RetrievalEngine` for each call. A cache
inside the engine is empty on every query.

```rust
// Don't: a HashMap field on the engine.

// Do (src/retrieval/signals.rs): `UsageCache` is a zero-sized handle on one
// static map, keyed by store, with a 30 s time to live.
pub struct UsageCache;
```

A failed read is cached as empty for the same time, so a broken log costs
one read per period, not one per query.

### 10. Use rayon only above a measured threshold, and only for a pure map

Why: rayon pays for itself only on large inputs. Most queries load a few
survivors, and entering the pool there saves nothing.

- Scoring: `PARALLEL_SCORE_MIN` = 64 (`src/retrieval/engine.rs`).
- Parsing in `get_batch`: `PARALLEL_PARSE_MIN` = 32
  (`crates/engram-storage/src/store/mod.rs`).
- The loop body must be a per-item map over read-only state, so the result
  order and values do not change.
- When the work is long, use `tokio::task::spawn_blocking`, not rayon on a
  tokio worker.

A separate threshold for one-shot hook processes was measured and gave no
gain. Do not add one.

## Allocations in per-row and per-line code

These rules apply to code that runs once per row, per line, per field or
per byte. On these paths a small allocation is multiplied by thousands.

### 11. Do not call `format!` per byte, per field or per line

Why: `format!("{b:02x}")` once per byte allocated a `String` per byte in 13
places. Its replacement cut `reindex` CPU by 16.5% (c5). A
`format!("**{field}:**")` per field per line in the memory-file parser was
the main cost of c4, which cut `reindex` allocations by 19.5%.

```rust
// Don't
digest.iter().map(|b| format!("{b:02x}")).collect::<String>()

// Do (crates/engram-types/src/hex.rs)
engram_types::hex::lower_hex(&digest)
```

```rust
// Don't
line.strip_prefix(&format!("**{field}:**"))

// Do (crates/engram-storage/src/memory_file/helpers.rs)
fn strip_field_marker<'a>(s: &'a str, field: &str) -> Option<&'a str> {
    s.strip_prefix("**")?.strip_prefix(field)?.strip_prefix(":**")
}
```

### 12. Use `as_str()` for enum names, not `Debug` plus `to_lowercase`

Why: `format!("{:?}", x).to_lowercase()` makes two strings for a constant.
`MemoryType`, `Status`, `Visibility` and `ProvenanceSource` have `as_str()`,
and a test pins each value to the old output.

```rust
// Don't
let type_str = format!("{:?}", m.type_).to_lowercase();

// Do (crates/engram-cli/src/commands/hook.rs)
let type_str = m.type_.as_str();
```

### 13. Borrow when the value is usually unchanged

Why: most values pass through a sanitizer unchanged. Returning `Cow`
removes a copy in the common case (c3, c8).

```rust
// Do (crates/engram-storage/src/memory_file/helpers.rs)
pub fn sanitize_single_line(value: &str) -> Cow<'_, str> {
    if !value.contains(['\n', '\r']) {
        return Cow::Borrowed(value);
    }
    // ...
}
```

Same pattern: `defang_harness_tags` in `src/ops/harvest.rs` skips the `replace`
when the marker is absent.

### 14. Write into one pre-sized buffer, not a `Vec` plus `join`

Why: intermediate `String`s and `Vec`s per section made up most of the
memory-file writer's allocations. Writing into one buffer cut `create` CPU
by 21% in that A/B (c3).

```rust
// Don't
let escaped = cleaned.lines().map(|l| f(l).into_owned()).collect::<Vec<_>>().join("\n");

// Do (src/ops/harvest.rs, `defang`)
let mut escaped = String::with_capacity(cleaned.len() + 16);
for (i, line) in cleaned.lines().enumerate() {
    if i > 0 {
        escaped.push('\n');
    }
    escaped.push_str(&escape_structural_line(line));
}
```

For several parts, use `write!` into one `String` (`class_entry_line` in
`hook.rs`), not `push_str(&format!(…))`.

When only part of the result is kept, do not build the whole. A tool result
with array content was joined into one `String` only to keep its first
line. `tool_result_preview` in `transcripts.rs` now takes the first
non-blank line of the first block that has one, which is the same line.

### 15. Compute a value once, outside the loop that tries options

Why: the hook budget loop re-defanged and re-rendered the whole entry for
every preview length it tried (c6).

```rust
// Do (crates/engram-cli/src/commands/hook.rs)
let Some(preview) = Preview::of(&scored.memory) else { continue };
// then preview.line(n) for each length n
```

### 16. Reuse a container across iterations; keep the iteration order

Why: the dashboard grid layout built a new `HashMap` and a `Vec` per cell on
every iteration. One map reused across iterations removed 73% of the layout
scenario's allocations (p2a).

```rust
// Do (crates/engram-dashboard/src/layout/grid.rs, `simulate`)
let mut buckets: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
for it in 0..iters {
    buckets.retain(|_, b| !b.is_empty());
    for b in buckets.values_mut() {
        b.clear();
    }
    // fill in ascending index order, as before
}
```

When the order of a floating-point sum depends on the container, keep the
same order. Otherwise the layout changes and `LAYOUT_VERSION` must change.

### 17. Keep short per-row strings inline, without changing the format on disk

Why: a text query parses the `keyword_stems` JSON of every candidate row.
One `String` per stem was about 64% of a text query's allocations.
`CompactString` keeps a stem of up to 24 bytes inline. It cut text-query
allocations by 59% at 1,000 memories (p2b). The JSON on disk did not
change, so no schema bump was needed.

Build the inline type at the source, not by converting `String`s after.
`KeywordStems::compute` first built `String` stems and a `String`-keyed map,
then copied every stem into a `CompactString` and rebuilt the map. Building
`CompactString` directly (`normalize_as` / `normalize_counts_as` in
`engram-types/src/normalize.rs`) removed 24% of `reindex` allocations and
15% of its CPU.

Before you add a dependency for this, check every feature set, not only the
default (`cargo tree -p engram-cli -e normal`, then again with
`--no-default-features --features ollama`). `compact_str` ships through
`tokenizers` in the default build. In the Ollama-only build it ships only
because `engram-types` uses it, and it is a third-party type in a public
field of that leaf crate. Name that cost in the change.

### 18. Do not build an error value on the success path

Why: chrono's `TimeDelta` deserializer builds an error message with
`ok_or(Error::custom(..))` on every successful parse: 2 allocations per
`half_life` or `ttl`, on every decayed row a query reads (p2c).

```rust
// Do (crates/engram-types/src/decay.rs, `lazy_delta`)
Duration::new(secs, nanos as u32)
    .map(Some)
    .ok_or_else(|| D::Error::custom("TimeDelta out of bounds"))
```

Use `ok_or_else`, `unwrap_or_else` and `map_or_else` when the fallback
allocates.

### 19. Build Arrow rows from borrowed strings

Why: `entries_to_batch` cloned every string into the Arrow builders (c7).
Push `&str` and `as_str()` instead. Arrow copies the bytes once anyway.

## Models, daemon and memory

### 20. Keep the ONNX run caps

Why: ONNX Runtime's CPU arena grows to the largest run it served and never
shrinks. One 64-chunk request left the daemon at about 3.4 GiB per pool
session.

- Embeddings: `MAX_TEXTS_PER_RUN` = 8
  (`crates/engram-models/src/embeddings/onnx.rs`).
- Reranker: `MAX_DOCS_PER_RUN` = 4 (`crates/engram-models/src/rerank.rs`).

Do not remove a cap. Do not hand fastembed a whole request.

### 21. Keep the allocator settings in the daemon

Why: glibc raises its mmap threshold each time a large block is freed.
After the first model unload, new weights came from the heap and the heap
grew from 19 MiB to about 105 MiB. `fix_mmap_threshold` (256 KiB) keeps it
at about 20 MiB. `malloc_trim(0)` after an unload returns the memory to the
system. Both are in `src/daemon/server.rs`. `M_ARENA_MAX` was tried and did
not help.

### 22. Never load a model inline on a hook path

Why: a hook runs on every tool call or prompt. A model load costs about
211 ms (embedding) and much more for the others.

- In the daemon, use `ProviderCache::embedding_for_hook`. It returns a model
  only when one is already loaded (`loaded_embedding`), and it starts a
  background load for the next prompt without waiting for it.
- Never spawn a daemon for a hook. Offer the hook to a daemon that is
  already running.
- Every branch must end in keywords at worst.
- `ProviderCache::get` loads every model a config enables. Do not call it
  on a hook path.

### 23. Batch embedding calls

Why: each embed call pays a mutex, a `spawn_blocking` hop, tokenizer setup
and, for the daemon, a socket round trip. Batching 64 texts took 511 ms to
208 ms (parallelization-simd.md).

```rust
// Don't
for t in &texts {
    provider.embed(t).await?;
}

// Do (src/retrieval/engine.rs): `RetrievalEngine::embed_texts`, which
// splits into chunks of EMBED_BATCH_CHUNK = 64 and calls embed_batch.
```

### 24. After touching the daemon's memory, run the footprint bench

When you change `ProviderCache`, the daemon request path or any ONNX
session setup, run `cargo bench --bench memory_footprint -- --check`. It
exits 1 when a scenario fails its bound.

## Dashboard

### 25. Layouts stay on the server, deterministic, and versioned

- The server computes every layout (`crates/engram-dashboard/src/layout/`).
  The page has no layout code.
- Use no random numbers. Loop in id order. Use only `+ − × ÷ sqrt` in
  positions.
- When a change alters any layout's output, bump `LAYOUT_VERSION` and update
  `PINNED` and `PINNED_AT` in `src/layout/tests.rs`. A performance change
  must not alter output, so it must pass
  `the_fixture_layouts_match_their_pinned_digests` unchanged.
- Keep `MAX_COMPUTING` (2) and `MAX_QUEUED` (16).

### 26. Keep the frontend rules that were paid for

Each of these was measured in
[dashboard-performance.md](./dashboard-performance.md). Do not undo one
without new numbers from the browser bench:

- No opacity transition on memories or relations.
- A display-filter toggle hides and shows built elements. It does not
  rebuild them.
- A style's `zoomed` runs once per settled view, never per frame.
- No `closePath` in batched relation paths. In Chrome, its cost grows with
  the length of the path: 10 ms against 0.7 ms on 10k points.
- At most two canvases in the page.
- No worker raster. An OffscreenCanvas worker took first paint from 53 ms
  to 96 ms.
- Grid stays pixel-identical (screenshot diff 0%).

## Build profile and SIMD

### 27. Measure at the profile that ships

`[profile.release]` is `opt-level = 3`. `[profile.bench]` matches it on
purpose. A mismatch between the two once let a 1.6× slowdown be merged.
Measure the whole workload, not only the kernel.

### 28. Use `fearless_simd` for `f32` reductions; write no `unsafe`

An `f32` sum does not auto-vectorize at any `opt-level`, because IEEE
addition is not associative. Follow `dot_unit_kernel` in
`src/ops/compress.rs`. Do not add `std::arch` intrinsics. Test every SIMD
level: extend the `simd-levels` CI job's filter when you add a kernel.

## Do not retry

These ideas were measured or reasoned through and rejected. Do not try them
again unless the stated condition has changed.

| idea | result | when it could return |
|---|---|---|
| Box `ScoreBreakdown.via` (c9) | Saves 40 bytes per breakdown. Allocation count did not change. CPU stayed in the noise. A public type change for no measurable gain. | Never as a performance change. |
| Content stem counts as a sorted `Vec` instead of a `HashMap` (p2d) | Peak heap −14 to −19%, but allocations +2.6 to +3.4% and CPU +7 to +21%. Existing stores are in hash order, so every row took the sort-and-copy fallback. | Only together with a schema bump that rewrites the stems sorted. |
| Box `IndexForFiltering.keyword_stems` | Not tried. It adds one allocation per row on every text query, the path p2b made free of per-stem allocations. | Never. |
| Reorder fields of `Memory` or `ScoredMemory` | They have no padding. Reordering does not shrink them. | Never. |
| Const size asserts on structs | No size is load-bearing. They only make new fields noisier. | When a size becomes load-bearing. |
| Pad atomics against false sharing | The shared atomics are touched once per daemon request, which costs milliseconds. Not measurable. | Never at the current request cost. |
| `read_msg` parsing from the reader's buffer | Saves 1 of 116 allocations per frame. | Never. |
| Cache each hook `Preview` across the budget rounds | The top slot builds its preview twice. Caching it saved 3 of ~27,600 allocations per PreToolUse hook, and CPU stayed in the noise. | Never. |
| Cache parsed `.md` files (in the daemon or per process) | The file read was 11% of a healthy query and is not hurt by fragments. It is the wrong stage to cache. Read only survivors instead (rule 7). | Measure the index projection first. |
| A scalar index as the fix for a per-item store loop | It removes the row filter only, not the per-call open and plan. | Never. Batch instead (rule 4). |
| Compaction on the `create` path | Added write latency and reordered the background ingest. | Never. It rides the read path. |
| A separate `PARALLEL_SCORE_MIN` for one-shot hook processes | No gain at 1,000 rows. The pool wins at 10,000 rows. | Never at these sizes. |
| A 1024-dimension embedding model | 7.8× on the query path, 10× on disk. Embedding was never the bottleneck. | See embedding-dimension-eval.md. |
| `M_ARENA_MAX` in the daemon | Did not help. `fix_mmap_threshold` did. | Never. |
| `[profile.release.package.*] opt-level` | Discarded under fat LTO (link-time optimization). | Never with fat LTO. |
| `-C target-cpu=native` | Under 10% gain, and the binary cannot ship as a prebuilt artifact. | Never for release builds. |
| `std::simd` | Nightly only. The repository is stable only. | When it is stable. |
| Parallel scoring in `keyword_search_stems` | A few hundred µs at 5,000 memories, below the surrounding I/O. | Never. |
| Dashboard: OffscreenCanvas worker raster, per-disc opaque tints, patch canvas in 64-px steps, `will-change` | Slower, visibly different, or no change. Details in dashboard-performance.md. | With new browser numbers only. |

## How to measure

### Which tool for which path

| path | tool | command |
|---|---|---|
| Allocations, heap, RSS, CPU of queries, hooks, writes, memory-file parse, digest, defang, daemon frames, layouts | `crates/engram-cli/examples/alloc_profile.rs` | see below |
| Query stage timings (embed, projection, vector search, file reads, whole engine) | `examples/query_stage_profile.rs` | `ENGRAMDB_DATA_DIR=… cargo run --release --example query_stage_profile -- <project-dir>`; add `KEYWORD_ONLY=1` to skip the model |
| Deterministic 1k / 2k / 10k stores for the above | `examples/dashboard_bench_store.rs` | `cargo run --example dashboard_bench_store -- <dir>` |
| Daemon and MCP resident memory | `benches/memory_footprint.rs` | `cargo bench --bench memory_footprint -- --check` |
| Parse, stems, scoring, cosine, embedding batches | `benches/parallel_simd.rs` | `cargo bench --bench parallel_simd` |
| Incremental `reindex` | `examples/reindex_incremental_bench.rs` | `cargo run --release --example reindex_incremental_bench` |
| Graph and usage scoring (quality and latency) | `examples/graph_scoring_eval.rs` | `cargo run --profile eval --example graph_scoring_eval` |
| Embedding model cost | `examples/embed_model_bench.rs` | see query-latency-profile.md R6 |
| Dashboard handlers and layouts | `crates/engram-dashboard/benches/dashboard_graph.rs` | `cargo bench -p engram-dashboard --bench dashboard_graph` |
| Dashboard page (paint, frames, toggles) | `crates/engram-dashboard/bench/browser/run.mjs`, `views.mjs` | see dashboard-performance.md "Rerun" |

### The allocation harness

```bash
# ORT and protoc as in .claude/CLAUDE.md, then:
cargo build --profile bench -p engram-cli --example alloc_profile
B=target/release/examples/alloc_profile
$B /tmp/allocm build                  # the persistent stores, once
$B /tmp/allocm all base.json          # every scenario
# interleaved A/B: copy each build's binary aside first
ALLOC_REPS=5 $B /tmp/allocm ab 3 out/ q-,hook- ./bin-before ./bin-after
$B /tmp/allocm compare out/bin-before.json out/bin-after.json
```

For a profile of where allocations come from, run
`valgrind --tool=dhat --dhat-out-file=x.json $B /tmp/allocm run <scenario>`.

### How to read the numbers

- Allocation counts are deterministic. They repeat exactly between runs of
  one binary (except `create` and `update`, about ±0.5%). Trust a change in
  allocation count.
- CPU varies ±4–19% across reps on queries and hooks. When a CPU change is
  smaller than the spread, report it as noise.
- Do not trust `create-1k` and `update-1k` CPU. Each rep adds fragments, so
  the cost per operation rises from rep to rep. Report only their
  allocation deltas.
- Compare only within one `ab` run. Close other heavy work first. Compare
  runs on one machine only.
- Fragmented stores are slower. When you time queries, say whether the
  store was compacted.

### Output pins

A performance change must pass all of these without regeneration:

- `retrieval::engine::text_query_pin_tests::text_query_output_is_pinned`:
  the full output of 288 text queries against a golden file. Do not set
  `ENGRAMDB_PIN_REGEN=1` for a performance change.
- `graph_signal_tests::default_config_ranking_is_unchanged_on_graph_fixture`
  and `default_config_reads_no_edges_or_usage` in
  `src/retrieval/engine.rs`.
- `the_fixture_layouts_match_their_pinned_digests` in
  `crates/engram-dashboard/src/layout/tests.rs`.
- The insta snapshots of the CLI (see [testing.md](./testing.md)). No
  `.snap` file may change.
- For a change to the memory-file writer: write stores with the binary
  before and after the change and run `diff -r` on the `memories/` trees.
  The bytes must be identical, because `content_sha256` is a digest of
  those bytes.

## Checklist before you finish a change on a hot path

1. Name the hot path the change touches (see [Hot paths](#hot-paths)).
2. Check the change against rules 1–28. Check it against
   [Do not retry](#do-not-retry).
3. If the change is meant to make code faster or smaller, measure it alone
   with the matching tool, in an interleaved A/B against the binary before
   it. Keep it only when the gain is larger than the noise.
4. Confirm that the output did not change: run the pins under
   [Output pins](#output-pins).
5. If you touched `ProviderCache`, the daemon request path or ONNX session
   setup, run `cargo bench --bench memory_footprint -- --check`.
6. If you added a dependency, run `cargo tree -p engram-cli -e normal` and
   confirm what new crate ships.
7. Record the numbers, kept or rejected, in the matching document.
8. Run `cargo fmt --all` and
   `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
   as for every change.
