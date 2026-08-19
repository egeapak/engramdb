# Where a query actually spends its time

*2026-08-19 · benchmark asset: `examples/query_stage_profile.rs`*

Measured to price a proposed daemon-side memory cache. The headline result is
not about the cache: **query latency is dominated by LanceDB fragment count, not
by row count, embedding inference, or file I/O.** A hundred ordinary `create`
calls make every scan in the store 4–7× slower, and stay that way until a
compaction pass runs.

Everything below is `target/release` on the web sandbox (Intel Xeon @ 2.80 GHz,
4 vCPU, AVX2), real ONNX `all-MiniLM-L12-v2-u8`, a real store on a real
filesystem. Note the release profile is `opt-level = "z"`, so these are the
numbers users actually get, not `opt-level = 3` numbers.

## TL;DR

| # | Finding | Consequence |
|---|---------|-------------|
| 1 | **Fragmentation, not scale, sets query cost.** At a *fixed* 400 rows, 100 uncompacted writes take `list_for_filtering` from 5.9 → 42.0 ms (7.1×) and `vector_search` from 3.5 → 15.7 ms (4.4×). | The cheapest large win in the product is compacting on fragment count, not only on a 6-hour timer. |
| 2 | **The file-read path is not hot, and does not fragment.** `get_batch(30)` is 2.2 ms and barely moves (1.3×) under the same fragmentation. | Caching parsed `.md` memories — the obvious "stop reading disk every time" fix — buys ~11% of a healthy query. It is the *wrong* thing to cache. |
| 3 | **Embedding inference is ~19% and already handled.** The forward pass is 3.8 ms; the 211 ms model *load* is what the daemon amortizes, and it already does. | No change needed. Raising the embedding dimension would attack the one term that is already cheap and already solved. |
| 4 | **`restrict_to` is a 2.5× pessimization on latency.** On a clean store `vector_search` is 3.5 ms unrestricted and 8.9 ms with a 400-id allowlist; dirty, 15.7 vs 53.3 ms. | It exists for *ranking correctness* (and has tests pinning that), so this is a real cost knowingly paid — but it should be measured, not assumed free. |

## Method

`examples/query_stage_profile.rs` times the four externally-observable stages of
the read path against a real store, using only public API:

- `provider.embed(q)` — the ONNX forward pass.
- `store.list_for_filtering()` — the whole post-predicate index projection
  (20 columns × N rows) streamed into Rust. Runs on **every** query, with no
  `limit`.
- `store.vector_search(v, 30, restrict)` — k-NN over the chunks table.
- `store.get_batch(&ids)` — two dirent scans plus one file read + parse per
  candidate.

Store: 300–400 memories seeded one at a time through `engramdb add`, which is
how a real store is built (an agent calling `create` per memory). ~2.5 chunks per
memory. 20 reps after a warm pass.

```bash
ORT_DYLIB_PATH=…/libonnxruntime.so \
  cargo run --release --example query_stage_profile -- <project-dir>
```

## Results

### R1 · A healthy 400-row store

| stage | p50 | share of query |
|---|---|---|
| `embed` (ONNX forward pass) | 3.76 ms | 19% |
| `list_for_filtering` (20 cols × 400 rows) | 5.89 ms | 30% |
| `vector_search` (restrict = 400 ids) | 8.88 ms | 45% |
| `get_batch(30)` (read + parse `.md`) | 2.21 ms | 11% |
| **total measured stage work** | **≈ 20.7 ms** | |

Once-per-process, not per query: store open 3.0 ms, **embedding model load
211 ms** — the term the daemon exists to amortize.

### R2 · The same store, after 100 ordinary `create` calls

Identical row count (400), identical data, no compaction in between
(`ENGRAMDB_DISABLE_AUTO_MAINTENANCE=1`):

| stage | compacted | +100 dirty writes | ratio |
|---|---|---|---|
| `list_for_filtering` | 5.89 ms | **41.96 ms** | **7.1×** |
| `vector_search(restrict=None)` | 3.52 ms | 15.65 ms | 4.4× |
| `vector_search(restrict=all ids)` | 8.88 ms | 53.30 ms | 6.0× |
| `list_ids()` (1 col × N) | 1.34 ms | 10.87 ms | 8.1× |
| `count()` (open + plan, no rows) | 0.32 ms | 0.80 ms | 2.5× |
| `get_batch(30)` (file I/O) | 2.21 ms | 2.93 ms | **1.3×** |
| `embed` | 3.76 ms | 4.18 ms | 1.1× |

A full query goes from **≈21 ms to ≈102 ms** — 5× — with no new data, purely
from uncompacted writes. Every LanceDB-touching stage degrades; the two stages
that do not touch LanceDB (`embed`, `get_batch`) are flat.

An earlier run on a store built by 300 sequential `add`s and never compacted
measured `list_for_filtering` at **141.5 ms** and `vector_search` at 61.3 ms —
24× and 19× the compacted figures. That run is what prompted this document, and
it is the state a write-heavy session leaves behind.

### R3 · Where the index-scan cost lives

| call | compacted 400 rows | reading |
|---|---|---|
| `count()` | 0.32 ms | table open + plan is cheap |
| `list_ids()` | 1.34 ms | 1 column × 400 rows |
| `list_for_filtering()` | 5.89 ms | 20 columns × 400 rows |

So the projection cost is per-column-per-row materialization, not per-call
overhead. Worth noting six of those columns (`physical`, `logical`, `tags`,
`watch_paths`, `audience`, `source_sessions`) are JSON-encoded strings that are
deserialized per row, per query.

### R4 · `get_batch` scaling

| candidates | p50 | per memory |
|---|---|---|
| 1 | 0.88 ms | 0.882 ms |
| 10 | 1.09 ms | 0.109 ms |
| 30 | 2.12 ms | 0.071 ms |
| 100 | 3.60 ms | 0.036 ms |
| 300 | 6.38 ms | 0.021 ms |

A ~0.9 ms fixed floor (the two dirent scans) plus ~0.02 ms per memory. The
marginal file read + parse is genuinely cheap; only the fixed scans are worth
attacking, and they are worth ~1 ms.

## What this means for a daemon-side cache

The motivation for caching in the **daemon** rather than per-MCP-process is
sound and is not in question here: the same project is routinely open in several
Claude Code sessions at once, so a per-process cache would hold N copies of one
project and give N independently-stale views. The daemon is the natural single
copy.

What the measurements change is **what to cache**, and how urgent it is:

- **Cache the index projection, not the memory files.** `list_for_filtering` is
  30% of a healthy query and 41% of a fragmented one; `get_batch` is 11% and
  1.3× immune to the thing that actually hurts. The original framing — "stop
  reading disk every time" — targets the cheap stage.
- **A cache makes queries fragmentation-proof**, which is worth more than its
  steady-state saving: it converts the 5× degradation in R2 into nothing. That
  is the strongest argument for it.
- **But compaction is far cheaper and fixes the same 5×.** `optimize()` already
  exists and is already called from `ops::maintenance`, whose comment
  anticipates precisely this ("a create/update-heavy workload that never runs
  `gc`/`reindex` grows disk monotonically"). It is throttled to
  `[maintenance].interval_secs`, default **6 hours**
  (`config.rs:1636`), and is skipped for hooks. So a session that writes 100
  memories can sit at 5× degraded latency for the rest of the window.

**Recommended order:**

1. **Trigger compaction on fragment count, not only elapsed time.** This is a
   small change to an existing throttled pass and recovers the entire 4–7×.
2. **Measure again.** If the compacted baseline (~21 ms) is good enough, the
   cache is optional.
3. **If a cache is still wanted, cache the index projection in the daemon**,
   validated against a cheap generation token rather than owned. Note the
   standing contract in CLAUDE.md — *"if the daemon is disabled or unreachable,
   MCP and the CLI load models in-process exactly as before; daemon failures
   must never break operations"* — which means the daemon may hold a **cache**,
   never the source of truth. It also means the daemon needs a new RPC family
   (its protocol is currently inference-only: `Ping / Meta / Embed / Classify /
   Rerank / Title / Status / Shutdown`) and a `PROTOCOL_VERSION` bump from `"3"`.

## Relation to the turbovec evaluation

[turbovec-evaluation.md](./turbovec-evaluation.md) rejected a quantized flat
index for the vector path. These numbers do not overturn that, but they sharpen
one point in it.

`vector_search` is 3.5–8.9 ms compacted and 15.7–53.3 ms fragmented, for a chunks
table of roughly a thousand rows. An in-RAM flat scan over the same vectors is
**sub-millisecond** — measured at 109–150 µs for n=2000 at 4-bit, and ~1 ms for a
naive f32 scalar loop. So the gap between "vectors in LanceDB" and "vectors in
RAM" is 1–2 orders of magnitude, and it is real.

But that gap is about **being in RAM**, not about quantization. At these row
counts a plain `Vec<f32>` scan captures essentially all of it, without
turbovec's 451 KB fixed overhead, its 12–25% top-1 loss at d=384, its `u64` id
constraint, or a fourth consistency obligation. If the daemon ever does hold
vectors resident, hold them as f32 first and measure before compressing
anything.

## Caveats

- **Synthetic corpus.** 400 templated memories over 12 topics. Real content is
  more varied, which affects embedding and scoring cost but not the
  fragmentation mechanism, which is structural.
- **One machine, 4 vCPU, `opt-level = "z"`.** Ratios should travel; absolute
  numbers will not.
- **Stage timings are measured externally**, by calling the same public API the
  engine calls, not by instrumenting `RetrievalEngine::query`. The engine's own
  `record_stage` histogram would be the authoritative source, but it is
  collected per-process and a one-shot CLI invocation exits before it flushes,
  so it is not readable from the outside today. Composite scoring and reranking
  are therefore not in the table above.
- **Fragment counts were not read directly** from LanceDB; causality is
  established by the before/after/after-recompaction sequence at fixed row
  count, not by counting fragments.
