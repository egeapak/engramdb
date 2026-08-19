# Where a query actually spends its time

*2026-08-19 · benchmark assets: `examples/query_stage_profile.rs`, `examples/embed_model_bench.rs`*

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
| 1 | **Fragmentation, not scale, sets query cost.** At a *fixed* 400 rows, 100 uncompacted writes take `list_for_filtering` from 5.9 → 42.0 ms (7.1×) and `vector_search` from 3.5 → 15.7 ms (4.4×). | **Fixed** — a fragment-count trigger now compacts without waiting for the 6-hour window, worth **4.0× on a full query** (R5). |
| 2 | **The file-read path is not hot, and does not fragment.** `get_batch(30)` is 2.2 ms and barely moves (1.3×) under the same fragmentation. | Caching parsed `.md` memories — the obvious "stop reading disk every time" fix — buys ~11% of a healthy query. It is the *wrong* thing to cache. |
| 3 | **Embedding inference is ~19% and already handled.** The forward pass is 3.8 ms; the 211 ms model *load* is what the daemon amortizes, and it already does. | No change needed. A 1024-dim model would cost **7.8× on the query path and 10× on disk** (R6) to widen the term that was already cheapest. |
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


## R5 · The fragment-pressure compaction trigger (implemented)

R2 said the fix was to compact on fragment count rather than only on a timer.
That is now `ops::maintenance::auto_compact`, wired into both front-ends.

**How it works.** The full maintenance pass stays on its 6-hour throttle,
because it does expensive work (a registry sweep and a store health scan).
Compaction gets its own, much cheaper trigger: stat one marker file, and only
if a short throttle has expired open the store, read fragment metadata, and run
`optimize()` if `small_fragments` is over the threshold. The common case costs a
single `stat` — no store open, no LanceDB call.

Two new `[maintenance]` knobs:

| knob | default | meaning |
|---|---|---|
| `compaction_fragment_threshold` | `32` | uncompacted fragments that make compaction due early. `0` disables the trigger, restoring purely time-based behaviour. |
| `compaction_min_interval_secs` | `120` | floor on how often the trigger may fire, so a write-heavy session cannot spin compaction on every command. |

Call sites: the CLI dispatch (beside `auto_maintain`, so every ordinary command
is an opportunity) and, on the MCP server, the **`query`** tool plus
`maintain_main_project`. Startup-only would have missed the case that matters,
since an MCP session is long-lived and writes throughout.

It deliberately rides the **read** path rather than the write path. Fragments
are produced by writes but only ever paid for by reads, so a query is both where
the cost lands and a point with nothing else in flight. Hanging it off `create`
was tried first and was wrong on two counts: it adds latency to the
agent-facing write, and that tool returns while a background ingest task is
still running (`embed_async: true`) which stamps the embedding fingerprint on
first embed — so *any* extra await between the write and the tool's return
reorders those two. `reindex_re_embeds_in_error_mode_despite_mismatch` catches
exactly that reordering, which is how the misplacement was found.

**Measured A/B.** 150 memories seeded one at a time, identical corpus, trigger
off vs on:

| stage | trigger **off** | trigger **on** | improvement |
|---|---|---|---|
| `list_for_filtering` | 95.66 ms | **26.64 ms** | **3.6×** |
| `vector_search(restrict=all ids)` | 85.24 ms | **14.25 ms** | **6.0×** |
| `vector_search` (as the engine calls it) | 47.44 ms | 9.10 ms | 5.2× |
| `list_ids()` | 19.53 ms | 3.60 ms | 5.4× |
| `count()` | 1.29 ms | 0.62 ms | 2.1× |
| `get_batch(30)` | 2.87 ms | 2.24 ms | 1.3× |
| `embed` | 7.63 ms | 4.89 ms | 1.6× (noise — it touches no LanceDB) |
| **full query** (embed + projection + restricted k-NN + get_batch) | **191.4 ms** | **48.0 ms** | **4.0×** |

The two stages that touch no LanceDB barely move, which is the control: the
trigger is acting on fragmentation and nothing else.

**Honest caveats.** The harness writes ~1 memory/second, roughly 60× faster
than a real session, so it was run with `compaction_min_interval_secs = 5`
rather than the shipped 120 — otherwise the trigger would get one firing for
work a real session spreads over an hour, and the A/B would measure the
throttle rather than the trigger. The shipped default is what a
realistically-paced session gets.

Also note the "on" column is still well above the 5.89 ms a *fully* compacted
400-row store reaches (R1/R3). That is by design: a threshold trigger tolerates
up to `compaction_fragment_threshold` fragments plus whatever arrived since the
last firing. Driving it to zero would mean compacting on every write.


## R6 · What a 1024-dim model would cost

The other proposed lever was a wider embedding. fastembed 5.2's catalogue tops
out at **1024 dims** (widths: 384, 512, 768, 1024), so 1536 is not selectable at
all without leaving the local/offline stack. The three quantized 1024-dim models
are now registered as selectable providers (`mxbai-embed-large-q`,
`gte-large-en-q`, `bge-large-en-q`) — none is a default.

Measured with `examples/embed_model_bench.rs` (same texts, same 256-token
chunking, 40 warm iterations, all models pre-staged, `ENGRAMDB_OFFLINE=1`):

| model | dims | warm p50 (query path) | ms/text at batch16 (create path) | ONNX on disk |
|---|---|---|---|---|
| **`all-MiniLM-L12-v2-u8`** *(shipped default)* | 384 | **3.72 ms** | **36.2** | **34 MB** |
| `all-MiniLM-L12-v2-q` | 384 | 4.53 ms | 36.0 | 33 MB |
| `all-MiniLM-L12-v2` fp32 | 384 | 6.71 ms | 55.2 | 128 MB |
| `mxbai-embed-large-v1-q` | 1024 | **29.07 ms** | 288.0 | 337 MB |
| `gte-large-en-v1.5-q` | 1024 | **32.32 ms** | 432.6 | 446 MB |
| `bge-large-en-v1.5-q` | 1024 | 199.17 ms | 898.9 | 638 MB |

Against the shipped default, the cheapest 1024-dim option costs **7.8× on the
query path, 8× on the create path, and 10× on disk** — and the daemon holds that
resident, machine-wide, for every project on the machine.

Put it in the query budget from R1: embedding goes 3.8 ms → 29.1 ms, taking a
healthy query from ~21 ms to ~46 ms. **Doubling query latency to widen vectors
that R5 just showed were never the bottleneck** is the wrong trade. `bge-large-q`
is worth flagging separately — its "-Q" repo ships `model_optimized.onnx`, which
is optimized fp32 rather than quantized, which is why it is 5–7× the other two.

This settles the open question in
[turbovec-evaluation.md](./turbovec-evaluation.md) R7. Widening the embedding
would rescue TurboQuant's recall at d=384 — but it is not worth doing for that
reason, or for any reason these measurements surface.

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

1. ~~Trigger compaction on fragment count, not only elapsed time.~~
   **Done — see R5.** Measured at 4.0× on a full query.
2. **Measure again in real use.** If the compacted baseline (~21–48 ms) is good
   enough, the cache is optional. That is now the open question.
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
