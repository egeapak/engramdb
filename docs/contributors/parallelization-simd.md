# Parallelization and SIMD

A survey of EngramDB's CPU-bound bulk paths, what each one costs, and which
ones were worth parallelizing or vectorizing. Everything here is measured;
`benches/parallel_simd.rs` is the harness and re-running it reproduces the
numbers.

```
cargo bench --bench parallel_simd
```

Numbers below: 4-core Intel Xeon @ 2.80GHz (AVX-512 capable), `profile.bench`
(`opt-level = 2`), rayon's default pool (4 threads). Treat the ratios as the
result, not the absolute times.

## Summary

| Path | Where | Before | After | Speedup |
|---|---|---|---|---|
| Pairwise cosine, 500 observations | `ops::compress::consolidation_pass` | 62.9 ms | 4.25 ms | **14.8×** |
| Single cosine, 384-dim | same | 496 ns | 114 ns | **4.4×** (no threads) |
| Memory-file parse, 5,000 | `MemoryStore::{reindex_dir, get_batch}` | 101.8 ms | 28.4 ms | **3.6×** |
| Keyword-stem derivation, 5,000 | `IndexEntry::from`, query fallback | 85.5 ms | 24.7 ms | **3.5×** |
| Composite scoring, 5,000 rows | `RetrievalEngine` score loops | 3.03 ms | 866 µs | **3.5×** |
| Reindex CPU (parse + stems), 5,000 | `MemoryStore::reindex_dir` | 219 ms | 64.2 ms | **3.4×** |

## Does rayon give you SIMD?

No. Rayon is thread-level data parallelism; it splits a range across a work
stealing pool and never affects the instructions in the loop body. SIMD comes
from LLVM's auto-vectorizer, and the two are independent — the 14.8× on the
pairwise pass is ~4.2× of vectorization multiplied by ~3.5× of rayon.

The blocker for auto-vectorization in Rust is not the target, it is IEEE
semantics. Floating-point addition is not associative, so a loop with one
running sum is a dependency chain LLVM is *not permitted* to reorder into
lanes, however wide the host is. Rust has no `-ffast-math`, so the fix is to
write the reassociation you want:

```rust
// stays scalar: one dependency chain
for (x, y) in a.iter().zip(b) { dot += x * y; }

// vectorizes: eight independent chains, one vector register
let mut lanes = [0.0f32; 8];
for (ac, bc) in a.chunks_exact(8).zip(b.chunks_exact(8)) {
    let ac: &[f32; 8] = ac.try_into().unwrap();
    let bc: &[f32; 8] = bc.try_into().unwrap();
    for l in 0..8 { lanes[l] += ac[l] * bc[l]; }
}
```

`chunks_exact` + `try_into` is load-bearing: it hands LLVM a `&[f32; 8]` whose
length is a compile-time constant, so the bounds checks disappear. The same
unrolling written as `&a[k*8..k*8+8]` measured *slower* than the scalar
version (327 ns vs 314 ns) — the slice has a runtime length, the bounds check
stays in the loop body, and nothing vectorizes. Both variants are kept in the
bench as `cosine_f32_lanes8` (slice indexing, does not vectorize) and
`cosine_f32_arr8` (array chunks, does).

## Register pressure decides more than lane width

`cosine_f32_arr8` computes three quantities — `Σab`, `Σa²`, `Σb²` — so eight
lanes each means 24 live vector lanes. That spills, and the vectorized version
lands at 314 ns, indistinguishable from scalar. Dropping to one accumulator set
takes it to 114 ns:

| variant | ns/pair | note |
|---|---|---|
| `f64_scalar` (what the code did) | 496 | widens every `f32` to `f64` |
| `f32_scalar` | 314 | narrower type, still one chain |
| `f32_lanes8` (slice indexing) | 327 | bounds checks block vectorization |
| `f32_arr8` (array chunks) | 314 | vectorizable, spills on 24 lanes |
| `dot_unit` (normalize first) | **114** | one accumulator set, stays in registers |

The same ordering holds on the full pairwise pass, which is what actually
runs — at 500 vectors: 63.4 ms today, 39.8 ms for `f32_arr8`, 14.6 ms once the
norms are hoisted, 4.11 ms with rayon on top.

Getting to one accumulator set is an algorithmic change, not a codegen trick:
L2-normalize each vector once in an O(n) prepass, and the O(n²) body is a bare
dot product. That also deletes real work — the old loop recomputed `‖a‖` and
`‖b‖` inside all n(n−1)/2 comparisons even though each vector has exactly one
norm. **Two thirds of the arithmetic was redundant**, and removing it is what
made the remaining third vectorizable.

No `unsafe`, no intrinsics, no `target_feature` gating, no runtime dispatch.
Building the probe with `-C target-cpu=native` on an AVX-512 host changed the
vectorized numbers by under 10%, which does not justify a per-architecture
intrinsic path.

## ⚠️ The release profile turns all of this off

`[profile.release]` sets `opt-level = "z"`. That disables LLVM's loop
vectorizer outright, so **the shipped binary gets none of the SIMD gain**.
Measured on the same machine with an isolated probe (three profiles, identical
source):

| | `opt-level = "z"` | `opt-level = 2` | `opt-level = 3` |
|---|---|---|---|
| dot product, 384-dim | 578 ns | 119 ns | **40 ns** |
| pairwise, 500 vectors | 94.4 ms | 14.6 ms | **6.4 ms** |
| pairwise speedup vs today | 1.6× | 4.3× | **9.2×** |

Two consequences:

1. `profile.bench` (`opt-level = 2`) does not model `profile.release`
   (`opt-level = "z"`). Every Criterion number in this repo is measured on a
   more aggressively optimized build than the one users run.
2. The threading gains are unaffected — rayon does not care about `opt-level`
   — but the vectorization gains largely evaporate in release.

`opt-level = "z"` was chosen for binary size, and most of this binary is
lance/datafusion/arrow/ort rather than EngramDB's own code, so a targeted

```toml
[profile.release.package.engramdb]
opt-level = 2
```

would recover the hot paths while leaving the dependency bulk at `z`. **This
has not been changed, and the binary-size cost has not been measured** — it is
a deliberate trade-off that belongs to whoever owns the release size budget.

## What was parallelized, and why those

Two properties make a loop a rayon candidate here: it is a pure per-item map
over shared read-only state (so results are order-preserving and identical),
and it is big enough that entering the pool pays for itself.

### `ops::compress::consolidation_pass` — pairwise similarity

The O(n²) heart of the consolidation pass, bounded at
`MAX_OBSERVATIONS_PER_PASS = 500` precisely because it was expensive. Now
normalize-once + vectorized dot + rayon over the outer index. Row `i` does
`n − i` comparisons, so the work is triangular; rayon's work stealing absorbs
that without manual chunking.

`similar_pairs` returns pairs in the same order the nested loops did — a test
asserts exact equality against the sequential reference, and a second test
asserts `dot_unit` agrees with the original `cosine` to 1e-5 across lengths
that do and do not divide by the lane count.

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

- **Wider lanes.** 16 `f32` lanes measured no better than 8 on an AVX-512
  host, and 8 is one AVX register / two SSE registers, so it degrades
  gracefully on older x86 and on NEON.
- **`-C target-cpu=native`.** Under 10% on the vectorized variants, and it
  makes the binary unshippable as a prebuilt artifact.
- **Explicit intrinsics / `std::simd`.** The portable `chunks_exact` form
  matches host-tuned codegen here. `std::simd` is nightly-only, and a
  runtime-dispatched intrinsic path would need per-architecture maintenance for
  no measured gain.
- **Parallelizing `keyword_search_stems`' scoring loop.** The stems come from
  the index now; the residual scoring is a few hundred µs at 5,000 memories and
  is dominated by the surrounding I/O.

## Not yet done

- **`consolidation_pass` embeds one observation at a time** (`engine.embed_text`
  in a `for` loop) rather than calling `embed_batch`. At the 500-observation
  cap that is 500 sequential model invocations, which is almost certainly a
  larger wall-clock cost than the entire pairwise pass this work optimized. It
  is a batching change, not a parallelism one, so it is out of scope here.
- **The `opt-level` question above**, which gates roughly half of the cosine
  gain in shipped builds.
