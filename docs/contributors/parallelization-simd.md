# Parallelization and SIMD

A survey of EngramDB's CPU-bound bulk paths, what each one costs, and which
ones were worth parallelizing. Everything here is measured;
`benches/parallel_simd.rs` is the harness and re-running it reproduces the
numbers.

```
cargo bench --bench parallel_simd
```

Numbers below: 4-core Intel Xeon @ 2.80GHz (AVX-512 capable), `profile.bench`
(`opt-level = 2`), rayon's default pool (4 threads). Treat the ratios as the
result, not the absolute times.

> **How this document evolved.** It started by measuring rayon over the bulk
> paths, then claimed the fast dot product was SIMD, then retracted that after
> disassembly showed it was instruction-level parallelism, then established
> that neither survives the release profile — and finally arrived at explicit
> intrinsics, which do. The dead ends are kept because each one is a trap
> someone will otherwise re-enter. If you only read one section, read
> [The release profile is the whole story](#the-release-profile-is-the-whole-story).

## Summary

| Path | Where | Before | After | Speedup |
|---|---|---|---|---|
| Single cosine, 384-dim, **in a release build** | `ops::compress::consolidation_pass` | 1126 ns | 55 ns | **20×** (no threads) |
| Pairwise cosine, 500 observations (bench profile) | same | 62.9 ms | 4.25 ms | **14.8×** |
| Memory-file parse, 5,000 | `MemoryStore::{reindex_dir, get_batch}` | 101.8 ms | 28.4 ms | **3.6×** |
| Keyword-stem derivation, 5,000 | `IndexEntry::from`, query fallback | 85.5 ms | 24.7 ms | **3.5×** |
| Composite scoring, 5,000 rows | `RetrievalEngine` score loops | 3.03 ms | 866 µs | **3.5×** |
| Reindex CPU (parse + stems), 5,000 | `MemoryStore::reindex_dir` | 219 ms | 64.2 ms | **3.4×** |
| Embedding 64 texts | `ops::compress::consolidation_pass` | 511 ms | 208 ms | **2.5×** |
| Reading 256 memories' vectors | `MemoryStore::export_chunks` | 13.48 s | 136 ms | **98.7×** (O(n²)→O(n)) |

## Does rayon give you SIMD?

No. Rayon is thread-level data parallelism: it splits a range across a
work-stealing pool and never affects the instructions in the loop body. Every
row in the table above except the cosine rows is purely rayon over an
already-existing loop body; the cosine rows are a separate story that took
three attempts to get right.

## What the lane trick actually does (a dead end, kept as a warning)

Rust's `f32` addition is IEEE-strict, so a single running sum is a dependency
chain the optimizer may not reassociate — each multiply-add waits on the
previous one, and the CPU's multiple FP units sit idle. Splitting the
accumulator into eight independent chains removes the dependency:

```rust
// one dependency chain: every add waits on the previous
for (x, y) in a.iter().zip(b) { dot += x * y; }

// eight independent chains: the CPU issues them in parallel
let mut lanes = [0.0f32; 8];
for (ac, bc) in a.chunks_exact(8).zip(b.chunks_exact(8)) {
    let ac: &[f32; 8] = ac.try_into().unwrap();
    let bc: &[f32; 8] = bc.try_into().unwrap();
    for l in 0..8 { lanes[l] += ac[l] * bc[l]; }
}
```

`chunks_exact` + `try_into` is load-bearing: it hands LLVM a `&[f32; 8]` whose
length is a compile-time constant, so the bounds checks disappear. The same
unrolling written as `&a[k*8..k*8+8]` measured *slower* than the scalar version
(327 ns vs 314 ns) — the slice has a runtime length and the bounds check stays
in the loop. Both variants are kept in the bench as `cosine_f32_lanes8` (slice
indexing, slow) and `cosine_f32_arr8` (array chunks, fast).

**But the eight chains stay scalar.** Disassembling the generated code settles
what LLVM actually emits for `dot_unit`:

| built at | instructions in `dot_unit` |
|---|---|
| `opt-level = 2` | 22 `addss`, 15 `mulss` — **all scalar**, zero packed |
| `opt-level = 3` | 3 `mulps`, 3 `addps` + scalar tail — genuinely vectorized |
| `opt-level = "z"` | neither: no unrolling, no vectorization |

So at `opt-level = 2` — which is `profile.bench`, i.e. every benchmark number
in this repo — the eight lanes are eight scalar `xmm` registers issued in
parallel. That is instruction-level parallelism. Only `opt-level = 3` turns
them into packed SSE.

This also re-explains the register-pressure result. `cosine_f32_arr8` computes
three quantities (`Σab`, `Σa²`, `Σb²`), so eight accumulators each means 24
live values against sixteen `xmm` registers — it spills, and lands at 314 ns,
indistinguishable from scalar. One accumulator set fits, and reaches 114 ns:

| variant | ns/pair | note |
|---|---|---|
| `f64_scalar` (what the code did) | 496 | widens every `f32` to `f64` |
| `f32_scalar` | 314 | narrower type, still one chain |
| `f32_lanes8` (slice indexing) | 327 | bounds checks block the unroll |
| `f32_arr8` (array chunks) | 314 | unrollable, but 24 accumulators spill |
| `dot_unit` (normalize first) | **114** | one accumulator set, stays in registers |

Getting to one accumulator set is an algorithmic change, not a codegen trick:
L2-normalize each vector once in an O(n) prepass and the O(n²) body is a bare
dot product. That also deletes real work — the old loop recomputed `‖a‖` and
`‖b‖` inside all n(n−1)/2 comparisons even though each vector has exactly one
norm. **Two thirds of the arithmetic was redundant**, and removing it is what
made the remaining third fit in registers.

The same ordering holds on the full pairwise pass, which is what actually runs
— at 500 vectors: 63.4 ms today, 39.8 ms for `f32_arr8`, 14.6 ms once the norms
are hoisted, 4.11 ms with rayon on top.

**None of this is what shipped.** Everything above is measured at
`opt-level = 2`, and the release profile is `opt-level = "z"`, where the
unrolled form is *slower than the naive loop*. The next section is what
actually ended up in the binary.

## The release profile is the whole story

`[profile.release]` sets `opt-level = "z"`, which runs neither the loop
vectorizer nor the unroller. Every arithmetic result in the section above
evaporates there — and the unrolled form is actively *worse* than the naive
loop, because the optimizer that would have cleaned it up is switched off.
Measured at `opt-level = "z"` with the real release profile (`lto = true`,
`codegen-units = 1`), 384-dim pairs:

| form | ns/pair | vs the unrolled version |
|---|---|---|
| plain scalar loop | 461 | **1.6× faster** |
| eight unrolled accumulators | 720 | 1.0× |
| `wide` crate (safe, portable SIMD wrapper) | 384 | 1.9× faster |
| **SSE2 intrinsics** | **65** | **11.0× faster** |
| **AVX2 + FMA intrinsics** | **55** | **13.1× faster** |

Two lessons, and the second is the one that matters.

### Benchmarks lie about this repo

`profile.bench` is `opt-level = 2`; `profile.release` is `opt-level = "z"`.
**Every Criterion number in this repo is measured on a more aggressively
optimized build than the one users run**, and for arithmetic-bound code the
two profiles do not even rank the candidates the same way. Tuning against
`cargo bench` alone produced a change that was 4.4× faster in the benchmark
and 1.6× *slower* in production. Rayon results are unaffected — thread-level
parallelism does not care about `opt-level` — but anything that depends on
codegen must be checked at `-Oz` before it is believed.

### Intrinsics work where auto-vectorization cannot

Auto-vectorization is an optimizer pass, so `-Oz` disables it. Intrinsics are
*semantic*: `_mm256_fmadd_ps` lowers to `vfmadd231ps` regardless of what the
optimizer is doing. That makes explicit SIMD the only way to get vector
arithmetic into a size-optimized build — the exact case where hand-written
SIMD earns its keep, and the opposite of the usual advice.

`ops::compress::dot_unit` therefore dispatches: AVX2+FMA when
`is_x86_feature_detected!` says so, SSE2 otherwise (x86-64 baseline, no
detection needed), NEON on aarch64 (mandatory there), single-accumulator
scalar elsewhere. Verified in the actual stripped release binary:
`dot_unit_avx2` contains `vfmadd231ps` and `vaddps`.

**Cost: 2,968 bytes.** 57,717,584 → 57,720,552 (+0.01%).

### Raising `opt-level` instead — built and rejected

The alternative is to let the auto-vectorizer do it, which means raising
`opt-level` across the whole dependency chain. Both levels were built and
measured end to end (everything else in the profile held fixed; speed is
`reindex --index-only` over a 1,200-memory store, 9 reps, interleaved):

| `opt-level` | binary | vs `"z"` | reindex min | reindex median | build |
|---|---|---|---|---|---|
| `"z"` | 55.04 MiB | — | 154 ms | 160 ms | 19m12s |
| `2` | 105.17 MiB | **+91.1%** | 119 ms | 127 ms | 35m57s |
| `3` | 110.83 MiB | **+101.4%** | 118 ms | 123 ms | 37m43s |

Roughly **double the binary for ~23% off a reindex** — and `3` over `2` buys
~3% more speed for another 5.7 MiB, so `2` is the knee. Against that, the
intrinsics get 11–13× on the arithmetic for 3 KB. The binary ships as a
prebuilt artifact (release archives, Homebrew, Scoop) and is spawned per
Claude Code hook invocation, so its size is download and cold-start cost, not
disk.

`reindex` improves far less than the microbenchmarks suggest because it is a
*mixed* workload — LanceDB commits, file I/O, allocator traffic — in which
arithmetic-bound code is a small slice.

### Per-package `opt-level` does not survive fat LTO

The tempting middle ground is to raise `opt-level` for only the hot crates:

```toml
[profile.release.package.engramdb]
opt-level = 2
```

**This was built, measured, and reverted.** A minimal two-crate reproduction
(`hotlib` at `opt-level = 2`, binary at `"z"`, fat LTO) produces a
**byte-identical binary** — same SHA-256 — to the same build with no override.
`cargo build -v` confirms rustc really is passed `-C opt-level=2`, so the
override is applied and then discarded: the fat-LTO link step re-runs the
pipeline at the top-level profile's `opt-level`. `lto = "thin"` behaves the
same. In the real binary the override changed the size slightly (+1.46%) but
disassembly found no trace of the `opt-level = 2` codegen shape in the hot
function.

Extending it to all seven workspace crates cost **+37.2%** (55.04 → 75.54 MiB)
— LTO inlining monomorphized dependency generics into our crates' codegen
units — for a `reindex` A/B of 196 ms min / 248 ms median baseline vs 192 ms /
201 ms overridden: inside the noise on the minimum.

Reindex would not have moved much regardless: its bulk work is frontmatter
deserialization and Snowball stemming, inside `toml`, `serde_yaml_ng` and
`rust-stemmers`, not in EngramDB's crates.

## Why not just use `fastembed::similarity`?

Reasonable question — `fastembed` is already in the tree and exports
`similarity::{dot, cosine_similarity, top_k}`. Three reasons, in order of how
decisive they are.

**1. It would not compile in every supported configuration.** `fastembed` is an
*optional* dependency of `engram-models`, gated behind the `onnxruntime`
feature, and the core `engramdb` crate deliberately carries no `fastembed`
dependency at all (see the layering rules in `.claude/CLAUDE.md`).
`consolidation_pass` lives in the core and must keep working under
`--no-default-features --features ollama`, where `fastembed` is not built. So a
`#[cfg]`-gated fallback would be needed regardless — meaning we would be
maintaining our own implementation *anyway*, plus a second code path.

**2. It is the shape we started from.** Its cosine is:

```rust
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()          // one dependency chain
}
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let na = dot(a, a).sqrt();                          // recomputed per call
    let nb = dot(b, b).sqrt();                          // recomputed per call
    if na == 0.0 || nb == 0.0 { return 0.0; }
    dot(a, b) / (na * nb)
}
```

Three `dot` calls per comparison, with both norms recomputed every time. In an
O(n²) loop where each vector has exactly one norm, that is the same 3×
redundancy `ops::compress::cosine` had before this work — and the single
accumulator chain does not vectorize at `-Oz`.

**3. Measured, at `opt-level = "z"`, 384-dim, identical results:**

| | ns/pair |
|---|---|
| `fastembed::similarity::cosine_similarity` | 1358 |
| ours, SSE2 | 68 |
| ours, AVX2 + FMA | **51** |

Adopting it would be a **26× regression** on this path. Nothing is wrong with
fastembed's version — it is a correct, readable convenience helper for scoring
a handful of candidates, which is what its `top_k` doc example does. It is just
not built for an all-pairs loop, and neither was ours until it was measured.

## Why not let LanceDB do the distance math?

LanceDB is already in the tree, already stores these vectors, and has tuned
SIMD distance kernels. For the query path it *is* what does the work — nothing
here reimplements vector search.

It is the wrong tool for `consolidation_pass` specifically:

- **Shape.** LanceDB answers "top-k nearest to *this* vector" over a large
  persisted index. The pass needs the full pairwise graph over ≤ 500 in-memory
  vectors. Getting that from LanceDB means ~500 separate searches, each paying
  query planning and I/O, against 125k dot products at ~51 ns ≈ 6 ms in-process.
- **Metric.** The chunks table uses LanceDB's default **L2** distance — nothing
  configures `DistanceType::Cosine` — which is why `lance_index` converts with
  `1.0 / (1.0 + distance)`. That is a monotone transform of L2, not cosine, so
  it is not interchangeable with the `[epistemic] consolidation_similarity`
  threshold users configure.

## Reading stored vectors instead of re-embedding

`consolidation_pass` calls `embed_text` on `summary + content` for every
observation — re-deriving vectors for memories that were **already embedded on
the write path** and are sitting in the chunks table. Reading them back should
obviously be cheaper than running a transformer. It was not, and finding out
why turned up a genuine O(n²).

### The per-memory read was quadratic

`MemoryStore::export_chunks` → `LanceIndex::chunks_for_memory` filters the
chunks table by `memory_id`. Every call re-opens the dataset (re-reading a
manifest whose fragment list grows with the table), builds its own query plan,
and scans. In a loop that is O(n²):

| n | per-memory loop | **per memory** |
|---|---|---|
| 16 | 91.1 ms | 5.69 ms |
| 32 | 298.9 ms | 9.34 ms |
| 64 | 922.3 ms | 14.4 ms |
| 128 | 3.477 s | 27.2 ms |
| 256 | 13.476 s | 52.6 ms |

Per-memory cost doubles on every doubling of n. Small samples are the cheap way
to see this — the shape is unmistakable by n=256, and confirming it at n=500
costs 65 s per sample.

### An index is *not* the fix

The obvious response is a scalar index on `chunks.memory_id`. It does not fix
this, and it is worth being precise about why: an index removes the row-filter
term only. It cannot remove the per-call table open, manifest read, or query
plan — and in a write-then-read pass it is stale for exactly the rows just
written, since LanceDB scans fragments added after the last `optimize` without
it. The shape stays `N × O(N)`.

### Batching is the fix

`LanceIndex::chunks_for_memories` collapses N opens/plans/scans into one (per
500-id sub-batch, matching `delete_chunks_batch`'s existing predicate bound).
That holds regardless of index state or fragmentation:

| n | per-memory | batched | speedup | per-memory cost: loop → batched |
|---|---|---|---|---|
| 16 | 91.1 ms | 7.73 ms | 11.8× | 5.69 ms → **0.48 ms** |
| 32 | 298.9 ms | 12.73 ms | 23.5× | 9.34 ms → **0.40 ms** |
| 64 | 922.3 ms | 25.81 ms | 35.7× | 14.4 ms → **0.40 ms** |
| 128 | 3.477 s | 57.36 ms | 60.6× | 27.2 ms → **0.45 ms** |
| 256 | 13.476 s | 136.5 ms | 98.7× | 52.6 ms → **0.53 ms** |

The last column is the result: **flat**. O(n²) → O(n), so the speedup grows
without bound rather than being a constant factor.

A BTree index on `chunks.memory_id` is still worth having and is now built
best-effort in `optimize()` — but for the lookups that are genuinely single-id
(`has_chunks`, `delete_chunks`, `vector_search`'s `restrict_to` prefilter), not
as the answer to the loop. BTree rather than Bitmap because `memory_id` has one
distinct value per memory; no schema-version bump, since an index adds no
column.

### So: read, or re-embed?

With the batch read in place, reading finally wins as expected — at n=64,
25.8 ms against 212 ms to embed the same 64 texts (**8×**), and the gap widens
with n because embedding is strictly linear while the read amortizes.

It is still **not a drop-in**, for reasons that are about behaviour rather than
speed:

| | re-embed (today) | stored chunks |
|---|---|---|
| text | `"{summary} {content}"` as one string | metadata row + content chunks |
| vectors per memory | 1 | 1 + N |
| long memories | **truncated** at the model's token limit | fully covered |
| title / tags | **absent** | included via the metadata row |
| unembedded memories | embedded on demand | absent, would be skipped |

Two of those are strict improvements (truncation, title/tag signal — the latter
is the composition the query path already uses, E1: MRR@10 0.75 → 0.89). One is
a real design decision: clustering needs one similarity per pair, but the
stored form is multi-vector, so the aggregation has to be chosen (max over
chunk pairs, matching query-path aggregation, is the obvious candidate). One is
a regression to handle: memories with no stored vectors drop out of
consolidation unless a fallback embeds them.

Left undone deliberately — it changes which clusters are found, which is a
product decision, not an optimization.

## Other per-item store calls in a loop

The same anti-pattern audited across the workspace. `export_chunks` was not the
worst instance:

| Location | N bounded by | When | Severity |
|---|---|---|---|
| `ops/reindex.rs:150` — `embed_memory` per memory | whole store | `reindex`, the documented fix for an embedding-fingerprint mismatch | **severe** — the `get_batch` at :145 is defeated one frame down by `store.get` inside `upsert_chunks_if_current` |
| `ops/gc.rs:179` — `delete_if` per candidate | ≤ whole store | `gc --confirm` | **high** — ~7 scans/commits per candidate |
| `worktree.rs:52` — `get`/`create`/`upsert_chunks` per stray file | stray `.md` files, usually 0 | first command inside a linked worktree | low — one-shot, and the cost is O(W × main store), not O(W²) |
| `ops/task.rs:127`, `ops/mod.rs:108`, `compress.rs:174`/`:230`, `cli/gc.rs:109` | tens | various | low — `get_batch` already exists for most of these |

None are fixed here. They are listed so the next person does not have to
rediscover them, and because the top two are worth doing before any further
micro-optimization of the arithmetic.

## What was parallelized, and why those

Two properties make a loop a rayon candidate here: it is a pure per-item map
over shared read-only state (so results are order-preserving and identical),
and it is big enough that entering the pool pays for itself.

### `ops::compress::consolidation_pass` — pairwise similarity

The O(n²) heart of the consolidation pass, bounded at
`MAX_OBSERVATIONS_PER_PASS = 500` precisely because it was expensive. Now
normalize-once + an intrinsics dot product + rayon over the outer index. Row
`i` does `n − i` comparisons, so the work is triangular; rayon's work stealing
absorbs that without manual chunking.

Hoisting the norms is the algorithmic half and it is worth stating on its own:
the old code recomputed `‖a‖` and `‖b‖` inside all n(n−1)/2 comparisons even
though each vector has exactly one norm, so **two thirds of the arithmetic was
redundant** before any codegen question arose.

`dot_unit_backends_agree` runs every backend against the scalar reference
across lengths straddling the 4/8/16-element strides. This is not ceremony:
any developer machine dispatches to AVX2, so without it the SSE2 path — what
every pre-Haswell CPU and every AVX2-masking VM executes — would ship
untested.

`similar_pairs` returns pairs in the same order the nested loops did — a test
asserts exact equality against the sequential reference, and a second test
asserts `dot_unit` agrees with the original `cosine` to 1e-5 across lengths
that do and do not divide by the accumulator count.

### `MemoryStore::reindex_dir` — parse + index-row construction

Restructured into phases: enumerate → read (bounded-concurrency async, as
before) → **parse in parallel** → resolve duplicate IDs (sequential, it is a
fold over a shared map, and it is cheap) → **build index rows in parallel**.

The two parallel phases are the two dominant costs: frontmatter
deserialization ~20 µs/memory, and the keyword-stem derivation inside
`IndexEntry::from` (tokenize → stoplist → Snowball stem → dedup, three fields)
~16 µs/memory.

### `MemoryStore::get_batch` — parse

Reads were already overlapped with `buffered(16)`; parsing still ran inline on
the tokio worker. Now factored into `parse_batch`, which goes through rayon at
or above `PARALLEL_PARSE_MIN = 32`. Below that it stays sequential — the
retrieval path calls `get_batch` with a handful of survivor IDs on most
queries, and paying pool-entry latency there to save nothing would be a
regression on the most common shape.

### `RetrievalEngine` — scoring and the keyword-stem fallback

Three loops, all guarded by `PARALLEL_SCORE_MIN = 64`:

- `rank_scope_only_from_index` — scores *every* index row. This is the
  no-query Rank shape the SessionStart/PreToolUse hooks take, where nothing
  narrows the candidate set first, so scoring is the whole cost rather than a
  tail behind file loading.
- the main scoring loop in `query`.
- the pre-v0.5.0 keyword-stem fallback, for stores not yet reindexed since the
  `keyword_stems` column landed.

Scope matching goes through `scope::physical`'s process-wide
`Mutex<HashMap<String, GlobMatcher>>` matcher cache, so this was the one place
lock contention could have eaten the gain. It does not: the cache's fast path
is a read under the mutex with no allocation, and the measured scaling
(3.5× on four cores at 5,000 rows) leaves no room for meaningful serialization.

### `consolidation_pass` — one batched embed instead of N

Not parallelism, but it was found by the same survey and it dwarfs everything
else in this document. The pass embedded its observations in a `for` loop, one
`embed_text` await each, even though every text is known up front and the
provider has always exposed `embed_batch`. Measured on the default quantized
all-MiniLM:

| texts | one at a time | batched | |
|---|---|---|---|
| 8 | 60.0 ms | 38.6 ms | 1.6× |
| 64 | 511 ms | 208 ms | **2.5×** |

Extrapolated to the pass's `MAX_OBSERVATIONS_PER_PASS = 500` cap, that is
roughly 4.0 s → 1.6 s. For scale: the work above took the similarity scan at
that size from 62.9 ms to 4.25 ms. **Embedding was, and after batching still
is, one to two orders of magnitude more wall-clock than the O(n²) scan it
feeds** — the scan was never the bottleneck, it was just the part that looked
like one.

What batching removes is per-invocation overhead paid N times: the provider
mutex, the `spawn_blocking` hop, tokenizer setup and ONNX session entry for the
local backend; a full HTTP round trip for Ollama; a full socket round trip for
the daemon. It also gives ONNX Runtime one padded batch to schedule instead of
N single-row matmuls.

`RetrievalEngine::embed_texts` chunks at `EMBED_BATCH_CHUNK = 64` — the ratio
is flat by then, the ONNX backend pads every row in a batch to the longest one,
and the daemon/Ollama backends put a whole batch in a single message. A failed
chunk falls back to per-text embedding, so one unembeddable text still costs
only itself.

## Rayon inside async

Every call site above already did this CPU work inline on a tokio worker
thread. Handing it to rayon blocks the calling worker for *less* time than
before, so this is strictly an improvement over the status quo — but it is not
the same thing as `spawn_blocking`, and it does not make the paths
cancellation-friendly. `reindex` runs under the per-project write lock and is
not latency-critical; the query paths are short enough that the difference does
not show up. If a future path parallelizes something long, route it through
`tokio::task::spawn_blocking` instead.

## Measured and rejected

- **`[profile.release.package.*] opt-level`** — see above. Silently discarded
  under fat LTO; +1.46% binary for one crate, +37.2% for all seven, no
  surviving speedup.
- **Raising `opt-level`** — see above. Roughly double the binary for ~23% off
  a reindex; the intrinsics get 11–13× on the arithmetic for 3 KB.
- **Hand-unrolled accumulators without intrinsics.** 1.6× *slower* than a
  naive loop in a release build. Only wins at `opt-level >= 2`.
- **The `wide` crate.** Safe, no `unsafe`, portable across x86 and aarch64 from
  one source, and it does emit `mulps`/`addps` at `-Oz` — but 384 ns/pair
  against 55 ns for intrinsics, because its abstraction layers do not get
  inlined without an optimizer. A good default in a normal profile; not in
  this one.
- **`std::simd`.** Nightly-only; the repo is stable-only.
- **`-C target-cpu=native`.** Under 10% on the auto-vectorized variants, and it
  makes the binary unshippable as a prebuilt artifact. Runtime dispatch gets
  the same instructions without giving that up.
- **Parallelizing `keyword_search_stems`' scoring loop.** The stems come from
  the index now; the residual scoring is a few hundred µs at 5,000 memories and
  is dominated by the surrounding I/O.

## If you add more arithmetic-bound code

1. Write it, and measure it at `opt-level = "z"` — not just via `cargo bench`,
   which uses `opt-level = 2` and will rank candidates differently.
2. If it is a reduction over a slice of `f32`/`f64`, expect auto-vectorization
   to give you nothing and reach for `std::arch` intrinsics with runtime
   dispatch, following `ops::compress::dot_unit`.
3. Disassemble the stripped release binary to confirm the instructions are
   there (`objdump -d | grep vfmadd`). Do not infer it from a benchmark.
4. Test every backend against a scalar reference, not just the one your
   machine dispatches to.
