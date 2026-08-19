//! Parallelization + arithmetic-throughput candidate measurements.
//!
//! Every group here pairs the shape the code uses today with one or more
//! candidate rewrites, so the speedup (or lack of one) is measured rather than
//! assumed. Nothing in this file is production code — the winners are ported
//! into the real call sites; the losers stay here as the record of why not.
//!
//! Run with `cargo bench --bench parallel_simd`.
//!
//! ## What is being tested
//!
//! | group | today | candidate |
//! |-------|-------|-----------|
//! | `parse` | `reindex_dir` / `get_batch` parse each `.md` in sequence | `rayon` over the file contents |
//! | `stems` | `KeywordStems::compute` per memory on the write/reindex path | `rayon` over memories |
//! | `score` | `composite_score_target` per index row (Rank path) | `rayon` over rows |
//! | `cosine` | `ops::compress::cosine` — one `f64` chain, widening every `f32` | `f32`, independent accumulator chains, `rayon` |
//!
//! ## Note on what the `cosine/*` variants measure
//!
//! `rayon` is *thread*-level parallelism only: it never changes the
//! instructions in the loop body. The `cosine/*` variants isolate a different
//! effect. Rust's `f32` addition is IEEE-strict, so a single running sum is a
//! dependency chain the optimizer may not reassociate — every multiply-add
//! waits on the previous one. Keeping eight independent accumulators lets the
//! CPU issue them in parallel.
//!
//! That is instruction-level parallelism, **not** SIMD: disassembly of the
//! generated code at `opt-level = 2` (this profile) shows eight scalar
//! `mulss`/`addss` chains and zero packed instructions. Only `opt-level = 3`
//! emits `mulps`/`addps`, and `[profile.release]`'s `opt-level = "z"` does
//! neither — so none of the `cosine/*` gain reaches a release binary. See
//! `docs/contributors/parallelization-simd.md`.

mod helpers;

use std::collections::HashMap;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use rayon::prelude::*;

use engramdb::scoring::{composite_score_target, ScoreTarget, ScoringContext};
use engramdb::storage::memory_file::{parse_memory_file, write_memory_file};
use engramdb::types::{EngramConfig, KeywordStems, Memory};

use helpers::generate_memory;

/// Store sizes swept by every group. 100 is a small project, 1_000 a mature
/// one, 5_000 the point where the O(n) query paths start to show.
const SIZES: &[usize] = &[100, 1_000, 5_000];

fn memories(n: usize) -> Vec<Memory> {
    (0..n).map(generate_memory).collect()
}

// ===========================================================================
// Group 1: memory-file parsing (reindex / get_batch)
// ===========================================================================

fn parse_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    group.sample_size(20);

    for &n in SIZES {
        let files: Vec<String> = memories(n)
            .iter()
            .map(|m| write_memory_file(m).expect("serialize"))
            .collect();

        group.bench_with_input(BenchmarkId::new("serial", n), &files, |b, files| {
            b.iter(|| {
                files
                    .iter()
                    .filter_map(|c| parse_memory_file(c).ok())
                    .collect::<Vec<_>>()
            });
        });

        group.bench_with_input(BenchmarkId::new("rayon", n), &files, |b, files| {
            b.iter(|| {
                files
                    .par_iter()
                    .filter_map(|c| parse_memory_file(c).ok())
                    .collect::<Vec<_>>()
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 2: keyword-stem derivation (write path + reindex + query fallback)
// ===========================================================================

fn stems_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("stems");
    group.sample_size(20);

    for &n in SIZES {
        let mems = memories(n);

        group.bench_with_input(BenchmarkId::new("serial", n), &mems, |b, mems| {
            b.iter(|| {
                mems.iter()
                    .map(|m| KeywordStems::compute(&m.summary, &m.tags, &m.content))
                    .collect::<Vec<_>>()
            });
        });

        group.bench_with_input(BenchmarkId::new("rayon", n), &mems, |b, mems| {
            b.iter(|| {
                mems.par_iter()
                    .map(|m| KeywordStems::compute(&m.summary, &m.tags, &m.content))
                    .collect::<Vec<_>>()
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 3: composite scoring over index rows (Rank path)
// ===========================================================================

fn score_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("score");
    group.sample_size(20);

    let config = EngramConfig::default();
    let now = chrono::Utc::now();
    let ctx_logical = ["retrieval.engine".to_string()];
    let ctx_path = "src/retrieval/engine.rs";

    for &n in SIZES {
        let mems = memories(n);

        group.bench_with_input(BenchmarkId::new("serial", n), &mems, |b, mems| {
            b.iter(|| {
                mems.iter()
                    .map(|m| {
                        let ctx = ScoringContext::scope_only(Some(ctx_path), &ctx_logical);
                        composite_score_target(ScoreTarget::from(m), &ctx, &config, now).final_score
                    })
                    .sum::<f64>()
            });
        });

        group.bench_with_input(BenchmarkId::new("rayon", n), &mems, |b, mems| {
            b.iter(|| {
                mems.par_iter()
                    .map(|m| {
                        let ctx = ScoringContext::scope_only(Some(ctx_path), &ctx_logical);
                        composite_score_target(ScoreTarget::from(m), &ctx, &config, now).final_score
                    })
                    .sum::<f64>()
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 4: cosine similarity — the arithmetic candidate
// ===========================================================================

/// Byte-for-byte the shape `ops::compress::cosine` uses today: one `f64`
/// accumulator chain, every `f32` widened on the way in.
fn cosine_f64_scalar(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Same math in `f32`. Still one dependent add chain, so still scalar — this
/// isolates "narrower type" from "reassociable loop".
fn cosine_f32_scalar(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        (dot as f64) / ((na as f64).sqrt() * (nb as f64).sqrt())
    }
}

/// Eight independent accumulators per quantity, indexed through a runtime
/// slice range. Each accumulator is its own dependency chain, which is the
/// reassociation IEEE-strict `f32` addition otherwise forbids — but `ax[l]`
/// on a slice of statically-unknown length keeps a bounds check in the loop
/// body, and three 8-wide sets need 24 live registers against sixteen `xmm`.
/// Both defeat the unroll. Kept as the measured counterexample to "just
/// unroll it".
fn cosine_f32_lanes8(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    const LANES: usize = 8;
    let mut dot = [0.0f32; LANES];
    let mut na = [0.0f32; LANES];
    let mut nb = [0.0f32; LANES];

    let chunks = a.len() / LANES;
    for k in 0..chunks {
        let ax = &a[k * LANES..k * LANES + LANES];
        let bx = &b[k * LANES..k * LANES + LANES];
        for l in 0..LANES {
            dot[l] += ax[l] * bx[l];
            na[l] += ax[l] * ax[l];
            nb[l] += bx[l] * bx[l];
        }
    }
    let (mut d, mut sa, mut sb) = (0.0f32, 0.0f32, 0.0f32);
    for l in 0..LANES {
        d += dot[l];
        sa += na[l];
        sb += nb[l];
    }
    for i in chunks * LANES..a.len() {
        d += a[i] * b[i];
        sa += a[i] * a[i];
        sb += b[i] * b[i];
    }
    if sa == 0.0 || sb == 0.0 {
        0.0
    } else {
        (d as f64) / ((sa as f64).sqrt() * (sb as f64).sqrt())
    }
}

/// The shape the optimizer can actually unroll: `chunks_exact` + `try_into`
/// gives LLVM a `&[f32; 8]` whose length is a compile-time constant, so the
/// bounds checks vanish.
///
/// Still computes all three quantities, so it still needs 24 live
/// accumulators. Measured against [`dot_unit_f32`] this is the "unrollable
/// loop, too much register pressure to stay unrolled" data point.
fn cosine_f32_arr8(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    const L: usize = 8;
    let (mut dot, mut na, mut nb) = ([0.0f32; L], [0.0f32; L], [0.0f32; L]);
    let mut ai = a.chunks_exact(L);
    let mut bi = b.chunks_exact(L);
    for (ac, bc) in ai.by_ref().zip(bi.by_ref()) {
        let ac: &[f32; L] = ac.try_into().expect("chunks_exact yields L elements");
        let bc: &[f32; L] = bc.try_into().expect("chunks_exact yields L elements");
        for l in 0..L {
            dot[l] += ac[l] * bc[l];
            na[l] += ac[l] * ac[l];
            nb[l] += bc[l] * bc[l];
        }
    }
    let (mut d, mut sa, mut sb) = (0.0f32, 0.0f32, 0.0f32);
    for l in 0..L {
        d += dot[l];
        sa += na[l];
        sb += nb[l];
    }
    for (x, y) in ai.remainder().iter().zip(bi.remainder()) {
        d += x * y;
        sa += x * x;
        sb += y * y;
    }
    if sa == 0.0 || sb == 0.0 {
        0.0
    } else {
        (d as f64) / ((sa as f64).sqrt() * (sb as f64).sqrt())
    }
}

/// A **rejected candidate**, kept for the comparison: eight accumulators, no
/// intrinsics, relying on the optimizer to unroll. Deliberately not in
/// production — at `opt-level = "z"` it is slower than a naive loop.
fn dot_unit_f32(a: &[f32], b: &[f32]) -> f64 {
    const L: usize = 8;
    let mut dot = [0.0f32; L];
    let mut ai = a.chunks_exact(L);
    let mut bi = b.chunks_exact(L);
    for (ac, bc) in ai.by_ref().zip(bi.by_ref()) {
        let ac: &[f32; L] = ac.try_into().expect("chunks_exact yields L elements");
        let bc: &[f32; L] = bc.try_into().expect("chunks_exact yields L elements");
        for l in 0..L {
            dot[l] += ac[l] * bc[l];
        }
    }
    let mut d: f32 = dot.iter().sum();
    for (x, y) in ai.remainder().iter().zip(bi.remainder()) {
        d += x * y;
    }
    d as f64
}

/// The production normalize and dot product, called directly.
///
/// These used to be copies. Production replaced the scalar norm with a SIMD
/// one and added an `is_finite` guard, and the copy did not follow — so every
/// "candidate" arm below was charged a prepass cost production no longer pays,
/// which flattered the rejected variants and penalised the shipped one. The
/// copies are gone; `cosine_pair/dot_unit` and every `intrinsics` arm now
/// measure exactly what `consolidation_pass` runs.
use engramdb::ops::compress::{dot_unit as dot_unit_prod, l2_normalized, similar_pairs};

/// A deterministic unit-ish 384-dim vector (all-MiniLM-L6-v2's dimension).
fn synth_vector(seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..384)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as f32 / (1u32 << 31) as f32) - 0.5
        })
        .collect()
}

fn cosine_benchmarks(c: &mut Criterion) {
    // --- single pair: isolates the arithmetic, no threading involved ---
    {
        let mut group = c.benchmark_group("cosine_pair");
        let a = synth_vector(1);
        let b_vec = synth_vector(2);

        let a_unit = l2_normalized(&a);
        let b_unit = l2_normalized(&b_vec);

        group.bench_function("f64_scalar", |b| b.iter(|| cosine_f64_scalar(&a, &b_vec)));
        group.bench_function("f32_scalar", |b| b.iter(|| cosine_f32_scalar(&a, &b_vec)));
        group.bench_function("f32_lanes8", |b| b.iter(|| cosine_f32_lanes8(&a, &b_vec)));
        group.bench_function("f32_arr8", |b| b.iter(|| cosine_f32_arr8(&a, &b_vec)));
        group.bench_function("dot_unit_candidate", |b| {
            b.iter(|| dot_unit_f32(&a_unit, &b_unit))
        });
        group.bench_function("dot_unit_SHIPPED", |b| {
            b.iter(|| dot_unit_prod(&a_unit, &b_unit))
        });
        group.finish();
    }

    // --- the real workload: the O(n^2) pairwise pass in `consolidation_pass` ---
    let mut group = c.benchmark_group("cosine_pairwise");
    group.sample_size(20);

    // 500 is `MAX_OBSERVATIONS_PER_PASS`, i.e. the worst case the pass admits.
    for &n in &[100usize, 500] {
        let vectors: Vec<Vec<f32>> = (0..n).map(|i| synth_vector(i as u64)).collect();
        let threshold = 0.9f64;

        group.bench_with_input(BenchmarkId::new("f64_scalar", n), &vectors, |b, v| {
            b.iter(|| {
                let mut pairs = Vec::new();
                for i in 0..v.len() {
                    for j in (i + 1)..v.len() {
                        if cosine_f64_scalar(&v[i], &v[j]) >= threshold {
                            pairs.push((i, j));
                        }
                    }
                }
                pairs.len()
            });
        });

        group.bench_with_input(BenchmarkId::new("f32_arr8", n), &vectors, |b, v| {
            b.iter(|| {
                let mut pairs = Vec::new();
                for i in 0..v.len() {
                    for j in (i + 1)..v.len() {
                        if cosine_f32_arr8(&v[i], &v[j]) >= threshold {
                            pairs.push((i, j));
                        }
                    }
                }
                pairs.len()
            });
        });

        // The candidate: O(n) normalize prepass, then an unrolled dot in the
        // O(n^2) body.
        group.bench_with_input(BenchmarkId::new("norm_dot", n), &vectors, |b, v| {
            b.iter(|| {
                let unit: Vec<Vec<f32>> = v.iter().map(|x| l2_normalized(x)).collect();
                let mut pairs = Vec::new();
                for i in 0..unit.len() {
                    for j in (i + 1)..unit.len() {
                        if dot_unit_f32(&unit[i], &unit[j]) >= threshold {
                            pairs.push((i, j));
                        }
                    }
                }
                pairs.len()
            });
        });

        // What actually ships — the real function, not a model of it.
        //
        // `similar_pairs` takes one Vec of chunks PER MEMORY and aggregates by
        // max over chunk pairs. The old bench arm modelled one vector per
        // memory, which understated the O(n²) body by the chunk factor and
        // measured a shape production had stopped using.
        let as_chunks: Vec<Vec<Vec<f32>>> = vectors.iter().map(|v| vec![v.clone()]).collect();
        group.bench_with_input(
            BenchmarkId::new("similar_pairs_SHIPPED_1chunk", n),
            &as_chunks,
            |b, v| b.iter(|| similar_pairs(v, threshold)),
        );

        // The realistic shape: a metadata row plus a content chunk, which is
        // what `metadata_vector = true` (the default) stores. k=2 means k_i*k_j
        // = 4 dot products per pair, so this is the number the maintenance
        // pass actually pays.
        let as_chunks2: Vec<Vec<Vec<f32>>> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| vec![v.clone(), synth_vector(i as u64 + 900_000)])
            .collect();
        group.bench_with_input(
            BenchmarkId::new("similar_pairs_SHIPPED_2chunk", n),
            &as_chunks2,
            |b, v| b.iter(|| similar_pairs(v, threshold)),
        );
    }

    group.finish();
}

// ===========================================================================
// Group 5: end-to-end shape of a reindex batch (parse + stems together)
// ===========================================================================

/// The two CPU-bound phases of `reindex_dir`, in the shape production uses.
///
/// Corrected three times over. It used to fuse parse and stems into one
/// `par_iter`, but `reindex_dir` runs them as two separate parallel passes with
/// a sequential duplicate-ID fold in between (it cannot fuse them — the fold
/// needs every parsed memory before any index row is built). Phase 5 is
/// `IndexEntry::for_file`, which is `KeywordStems::compute` *plus* ~25 field
/// clones; measuring only the stems under-reported it. And phase 3 now hashes
/// each file's bytes (schema 0.8.0 content digests) alongside the parse, on the
/// same rayon pass — omitting that would under-report the phase whose cost
/// decided the whole incremental-reindex phasing.
fn reindex_cpu_benchmarks(c: &mut Criterion) {
    use engramdb::storage::digest::FileDigest;
    use engramdb::storage::lance_index::IndexEntry;

    let mut group = c.benchmark_group("reindex_cpu");
    group.sample_size(10);

    for &n in SIZES {
        let files: Vec<String> = memories(n)
            .iter()
            .map(|m| write_memory_file(m).expect("serialize"))
            .collect();

        group.bench_with_input(BenchmarkId::new("serial", n), &files, |b, files| {
            b.iter(|| {
                // Phase 3 equivalent: parse + content digest, one pass.
                let parsed: Vec<(Memory, FileDigest)> = files
                    .iter()
                    .filter_map(|c| {
                        parse_memory_file(c)
                            .ok()
                            .map(|m| (m, FileDigest::of(c.as_bytes())))
                    })
                    .collect();
                // Phase 4: sequential dedup fold (cheap, but it is the barrier
                // that stops phases 3 and 5 being fused).
                let by_id: HashMap<String, (Memory, FileDigest)> =
                    parsed.into_iter().map(|p| (p.0.id.clone(), p)).collect();
                // Phase 5 equivalent: build the index rows.
                let entries: Vec<IndexEntry> = by_id
                    .values()
                    .map(|(m, d)| IndexEntry::for_file(m, d))
                    .collect();
                entries.len()
            });
        });

        group.bench_with_input(BenchmarkId::new("rayon", n), &files, |b, files| {
            b.iter(|| {
                let parsed: Vec<(Memory, FileDigest)> = files
                    .par_iter()
                    .filter_map(|c| {
                        parse_memory_file(c)
                            .ok()
                            .map(|m| (m, FileDigest::of(c.as_bytes())))
                    })
                    .collect();
                let by_id: HashMap<String, (Memory, FileDigest)> =
                    parsed.into_iter().map(|p| (p.0.id.clone(), p)).collect();
                let owned: Vec<&(Memory, FileDigest)> = by_id.values().collect();
                let entries: Vec<IndexEntry> = owned
                    .par_iter()
                    .map(|(m, d)| IndexEntry::for_file(m, d))
                    .collect();
                entries.len()
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 6: embedding one text at a time vs. one batched call
// ===========================================================================

/// `consolidation_pass` embeds its observations in a `for` loop, one
/// `embed_text` await per memory, even though the provider exposes
/// `embed_batch` and every text is known up front.
///
/// This measures what that costs. Both arms do the same total number of
/// embeddings — the only difference is how many times the model is invoked.
/// A batched call amortizes the per-invocation overhead (lock acquisition,
/// `spawn_blocking` hop, tokenizer setup, ONNX session entry, output tensor
/// allocation) and lets ONNX Runtime schedule the batch as one padded matmul
/// instead of N separate ones.
///
/// Requires the embedding model in the local cache and a resolvable ONNX
/// Runtime; skipped with a notice otherwise, so it never fails a bench run on
/// a machine without them.
#[cfg(feature = "onnxruntime")]
fn embed_batching_benchmarks(c: &mut Criterion) {
    use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};

    let Some(provider) = OnnxProvider::try_new() else {
        eprintln!("embed_batching: embedding model unavailable, skipping");
        return;
    };

    let mut group = c.benchmark_group("embed_batching");
    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(10));

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let corpus: Vec<String> = (0..64)
        .map(|i| {
            format!(
                "Observation {i}: the retrieval engine filters candidates at the index \
                 level before scoring them, so the vector search only runs over rows \
                 that already passed the hard filters."
            )
        })
        .collect();

    // 8 is a small consolidation cluster; 64 is a busy maintenance pass.
    // `MAX_OBSERVATIONS_PER_PASS` is 500, but benching that many single
    // embeds takes minutes and the ratio is already flat by 64.
    for &n in &[8usize, 64] {
        let texts: Vec<&str> = corpus.iter().take(n).map(String::as_str).collect();

        group.bench_with_input(BenchmarkId::new("sequential", n), &texts, |b, texts| {
            b.to_async(&rt).iter(|| async {
                let mut out = Vec::with_capacity(texts.len());
                for t in texts {
                    out.push(provider.embed(t).await.expect("embed"));
                }
                out
            });
        });

        group.bench_with_input(BenchmarkId::new("batched", n), &texts, |b, texts| {
            b.to_async(&rt)
                .iter(|| async { provider.embed_batch(texts).await.expect("embed_batch") });
        });
    }

    group.finish();
}

#[cfg(not(feature = "onnxruntime"))]
fn embed_batching_benchmarks(_c: &mut Criterion) {}

// ===========================================================================
// Group 7: re-embedding vs reading the vectors already in the chunk table
// ===========================================================================

/// `consolidation_pass` calls `embed_text` on every observation, deriving
/// vectors for memories that were **already embedded on the write path** and
/// whose vectors are sitting in the LanceDB chunk table.
///
/// This measures the alternative: `MemoryStore::export_chunks` per memory.
/// Synthetic vectors are written directly with `upsert_chunks`, so the bench
/// needs no embedding model and isolates the storage read.
///
/// It is a read-vs-inference comparison, not a drop-in swap — the two produce
/// *different vectors* (see the feature notes in
/// `docs/contributors/parallelization-simd.md`). The point is the order of
/// magnitude between them.
fn chunk_read_benchmarks(c: &mut Criterion) {
    use engramdb::storage::{InMemoryRegistry, MemoryStore};

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("chunk_read");
    group.sample_size(10);

    // Small samples on a geometric sweep: the point is the SHAPE, not the
    // absolute time. A per-memory cost that doubles as n doubles is O(n^2)
    // overall; a flat one is O(n). Reading the curve off 16..256 is far
    // cheaper than confirming it at 500 (65 s per sample).
    for &n in &[16usize, 32, 64, 128, 256] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (store, ids) = rt.block_on(async {
            let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
                .await
                .expect("init");
            let mut ids = Vec::with_capacity(n);
            for i in 0..n {
                let m = generate_memory(i);
                store.create(&m).await.expect("create");
                // Two rows per memory, matching the default metadata_vector
                // composition (one metadata row + one content chunk).
                store
                    .upsert_chunks(
                        &m.id,
                        vec![synth_vector(i as u64), synth_vector(i as u64 + 1_000_000)],
                    )
                    .await
                    .expect("upsert_chunks");
                ids.push(m.id.clone());
            }
            (store, ids)
        });

        // Per-memory: N table opens, N manifest reads, N query plans, N scans.
        group.bench_with_input(BenchmarkId::new("per_memory", n), &ids, |b, ids| {
            b.to_async(&rt).iter(|| async {
                let mut total = 0usize;
                for id in ids {
                    total += store.export_chunks(id).await.expect("export").len();
                }
                total
            });
        });

        // Batched: one open, one plan, one scan for the whole set. If the
        // per-memory cost of this arm stays flat as n doubles while the arm
        // above doubles, the complexity really changed and it is not a
        // constant-factor win.
        group.bench_with_input(BenchmarkId::new("batched", n), &ids, |b, ids| {
            b.to_async(&rt).iter(|| async {
                store
                    .export_chunks_batch(ids)
                    .await
                    .expect("export batch")
                    .len()
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 8: the batched store primitives behind reindex / gc
// ===========================================================================

/// Per-memory `upsert_chunks` against its batched replacement, swept over
/// small n so the SHAPE is visible: a per-memory cost that grows with n is
/// quadratic, a flat one is linear.
///
/// The store is built once per n and reused across iterations — `merge_insert`
/// makes a repeat upsert an update rather than an insert, and the cost of
/// interest (table opens, commits, manifest reads, the `has_embedding` update)
/// is the same either way.
fn store_batch_benchmarks(c: &mut Criterion) {
    use engramdb::storage::{InMemoryRegistry, MemoryStore};

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("store_batch");
    group.sample_size(10);

    for &n in &[16usize, 32, 64, 128] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (store, mems) = rt.block_on(async {
            let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
                .await
                .expect("init");
            let mut mems = Vec::new();
            for i in 0..n {
                let m = generate_memory(i);
                store.create(&m).await.expect("create");
                mems.push(m);
            }
            (store, mems)
        });

        group.bench_with_input(BenchmarkId::new("upsert_per_memory", n), &n, |b, _| {
            b.to_async(&rt).iter(|| async {
                for (i, m) in mems.iter().enumerate() {
                    store
                        .upsert_chunks(&m.id, vec![synth_vector(i as u64)])
                        .await
                        .expect("upsert");
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("upsert_batched", n), &n, |b, _| {
            b.to_async(&rt).iter(|| async {
                let entries: Vec<_> = mems
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (m.id.clone(), m.updated_at, vec![synth_vector(i as u64)]))
                    .collect();
                store.upsert_chunks_batch(entries).await.expect("batch");
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 9: the per-memory update loop (task complete / supersedes close)
// ===========================================================================

/// `MemoryStore::update_with` in a loop, swept over small n.
///
/// Sizing the remaining batching candidates before changing them:
/// `ops::task::complete_task` and `ops::mod::close_superseded_windows` both
/// call a per-memory mutating primitive in a loop, and each one is a write
/// lock + a directory scan + an index upsert + a full manifest-stats scan. If
/// the per-memory cost grows with n here, batching is a complexity change and
/// worth doing; if it is flat, it is a constant factor and probably is not.
fn store_update_benchmarks(c: &mut Criterion) {
    use engramdb::storage::{InMemoryRegistry, MemoryStore};

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("store_update");
    group.sample_size(10);

    for &n in &[16usize, 32, 64, 128] {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (store, ids) = rt.block_on(async {
            let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
                .await
                .expect("init");
            let mut ids = Vec::new();
            for i in 0..n {
                let m = generate_memory(i);
                store.create(&m).await.expect("create");
                ids.push(m.id.clone());
            }
            (store, ids)
        });

        group.bench_with_input(BenchmarkId::new("update_per_memory", n), &n, |b, _| {
            b.to_async(&rt).iter(|| async {
                for id in &ids {
                    store
                        .update_with(id, |m| {
                            m.criticality = 0.5;
                            Ok(())
                        })
                        .await
                        .expect("update");
                }
            });
        });

        // N single reads vs one batched read — the shape behind the
        // `supersedes` / `compress` / pretty-`gc` loops.
        group.bench_with_input(BenchmarkId::new("get_per_memory", n), &n, |b, _| {
            b.to_async(&rt).iter(|| async {
                for id in &ids {
                    store.get(id).await.expect("get");
                }
            });
        });
        group.bench_with_input(BenchmarkId::new("get_batched", n), &n, |b, _| {
            b.to_async(&rt)
                .iter(|| async { store.get_batch(&ids).await.expect("get_batch") });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    parse_benchmarks,
    stems_benchmarks,
    score_benchmarks,
    cosine_benchmarks,
    reindex_cpu_benchmarks,
    embed_batching_benchmarks,
    chunk_read_benchmarks,
    store_batch_benchmarks,
    store_update_benchmarks,
);
criterion_main!(benches);
