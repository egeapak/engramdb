# Parallelization and instruction-level parallelism

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

> **Note on the title.** An earlier draft of this document called the dot
> product work "SIMD". Disassembly says otherwise — see
> [What the lane trick actually does](#what-the-lane-trick-actually-does). The
> speedups are real and reproducible; the mechanism is instruction-level
> parallelism, and the distinction changes what you should do next.

## Summary

| Path | Where | Before | After | Speedup |
|---|---|---|---|---|
| Pairwise cosine, 500 observations | `ops::compress::consolidation_pass` | 62.9 ms | 4.25 ms | **14.8×** |
| Single cosine, 384-dim | same | 496 ns | 114 ns | **4.4×** (no threads) |
| Memory-file parse, 5,000 | `MemoryStore::{reindex_dir, get_batch}` | 101.8 ms | 28.4 ms | **3.6×** |
| Keyword-stem derivation, 5,000 | `IndexEntry::from`, query fallback | 85.5 ms | 24.7 ms | **3.5×** |
| Composite scoring, 5,000 rows | `RetrievalEngine` score loops | 3.03 ms | 866 µs | **3.5×** |
| Reindex CPU (parse + stems), 5,000 | `MemoryStore::reindex_dir` | 219 ms | 64.2 ms | **3.4×** |
| Embedding 64 texts | `ops::compress::consolidation_pass` | 511 ms | 208 ms | **2.5×** |

## Does rayon give you SIMD?

No, and neither does the code below. Rayon is thread-level data parallelism: it
splits a range across a work-stealing pool and never affects the instructions
in the loop body. Everything in the table above except the cosine rows is
purely rayon over an already-existing loop body.

## What the lane trick actually does

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

No `unsafe`, no intrinsics, no `target_feature` gating, no runtime dispatch.
Building the probe with `-C target-cpu=native` on an AVX-512 host changed the
numbers by under 10%, which does not justify a per-architecture intrinsic path.
If someone later wants genuine SIMD here, `opt-level = 3` on the whole release
profile — not explicit intrinsics — is the first thing to measure.

## ⚠️ The release profile, and why the obvious fix does not work

`[profile.release]` sets `opt-level = "z"`, which does neither the unrolling
nor the vectorization. So the shipped binary gets **none** of the 4.4×. Held
against the same profile (`lto`, `codegen-units`, `panic`, `strip` all
identical) with only `opt-level` varied:

| | `opt-level = "z"` | `opt-level = 2` | `opt-level = 3` |
|---|---|---|---|
| `dot_unit`, one 384-dim pair | 624 ns | 146 ns | **70 ns** |
| pairwise scan, n=500, old code | 142.1 ms | 75.6 ms | 63.6 ms |
| pairwise scan, n=500, new code | 80.4 ms | 19.4 ms | **9.8 ms** |
| new vs old, same profile | 1.8× | 3.9× | 6.5× |

The obvious fix is a per-package override, so only EngramDB's crates build for
speed while the lance/datafusion/arrow/ort bulk stays at `"z"`:

```toml
[profile.release.package.engramdb]
opt-level = 2
```

**This was built, measured, and reverted. It does not work under `lto = true`.**

- A minimal two-crate reproduction (`hotlib` at `opt-level = 2`, binary at
  `"z"`, fat LTO) produces a **byte-identical binary** — same SHA-256 — to the
  same build with no override at all. `cargo build -v` confirms rustc really is
  passed `-C opt-level=2` for the overridden crate, so the override is applied
  and then discarded: the fat-LTO link step re-runs the optimization pipeline
  at the top-level profile's `opt-level`. `lto = "thin"` behaves the same.
- In the real binary, the override did change the output slightly (57,717,584 →
  58,559,088 bytes, +1.46%) but disassembly finds **no trace of the
  `opt-level = 2` codegen shape** in the hot function: the unrolled eight-chain
  pattern (a dense run of `mulss`) appears zero times in either binary.
- Extending the override to all seven workspace crates cost **+21,494,600 bytes
  (+37.2%, 55.04 → 75.54 MiB)** — LTO inlining monomorphized dependency
  generics into our crates' codegen units — for no speedup that survived
  measurement. An interleaved A/B of `reindex --index-only` over 1,200 memories
  gave 196 ms min / 248 ms median baseline vs 192 ms min / 201 ms median
  overridden: inside the noise on the minimum.

Reindex would not have moved much regardless: the bulk work on that path is
frontmatter deserialization and Snowball stemming, which happen inside `toml`,
`serde_yaml_ng` and `rust-stemmers`, not in EngramDB's crates.

**What is left:** raising `opt-level` on `[profile.release]` itself is the only
change that would reach the hot code, and its binary-size cost has not been
measured. That is a deliberate trade-off belonging to whoever owns the release
size budget, so nothing here changes the profile.

A second consequence worth internalizing: `profile.bench` (`opt-level = 2`)
does not model `profile.release` (`opt-level = "z"`). **Every Criterion number
in this repo is measured on a more aggressively optimized build than the one
users run.** The rayon gains are unaffected — thread-level parallelism does not
care about `opt-level` — but the single-thread arithmetic gains are.

## What was parallelized, and why those

Two properties make a loop a rayon candidate here: it is a pure per-item map
over shared read-only state (so results are order-preserving and identical),
and it is big enough that entering the pool pays for itself.

### `ops::compress::consolidation_pass` — pairwise similarity

The O(n²) heart of the consolidation pass, bounded at
`MAX_OBSERVATIONS_PER_PASS = 500` precisely because it was expensive. Now
normalize-once + unrolled dot + rayon over the outer index. Row `i` does
`n − i` comparisons, so the work is triangular; rayon's work stealing absorbs
that without manual chunking.

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
- **Wider accumulators.** 16 chains measured no better than 8, and 8 fits the
  sixteen `xmm` registers with room for the loop's other live values.
- **`-C target-cpu=native`.** Under 10%, and it makes the binary unshippable as
  a prebuilt artifact.
- **Explicit intrinsics / `std::simd`.** `std::simd` is nightly-only, and a
  runtime-dispatched intrinsic path would need per-architecture maintenance.
  Worth revisiting only if `opt-level` on the release profile is ruled out —
  raising that is strictly less work for the same or better result.
- **Parallelizing `keyword_search_stems`' scoring loop.** The stems come from
  the index now; the residual scoring is a few hundred µs at 5,000 memories and
  is dominated by the surrounding I/O.

## Not yet done

- **Raising `opt-level` on `[profile.release]` itself**, the only change that
  reaches the hot arithmetic in shipped builds. Needs a binary-size measurement
  before anyone commits to it.
