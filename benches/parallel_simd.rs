//! Parallelization / SIMD candidate measurements.
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
//! | `cosine` | `ops::compress::cosine` — scalar, widening every `f32` to `f64` | `f32` accumulators, unrolled lanes, `rayon` |
//!
//! ## Note on SIMD
//!
//! `rayon` is *thread*-level parallelism only: it never emits vector
//! instructions. SIMD comes from LLVM auto-vectorization, which needs (a) a
//! loop LLVM can prove is reassociable and (b) a target that has the
//! instructions. Rust's default `f32`/`f64` addition is IEEE-strict, so a
//! plain accumulator loop is *not* reassociable and stays scalar no matter how
//! wide the host is. The `cosine/*` variants below isolate that effect: the
//! `lanes8` variants keep eight independent accumulators, which gives LLVM the
//! reassociation for free without `-ffast-math`-style flags.

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
// Group 4: cosine similarity — the SIMD candidate
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
/// slice range. Each lane is its own dependency chain, which is the
/// reassociation IEEE-strict `f32` addition otherwise forbids — but `ax[l]`
/// on a slice of statically-unknown length keeps a bounds check in the loop
/// body, and three 8-wide accumulator sets need 24 live vector lanes. Both
/// block vectorization. Kept as the measured counterexample to "just unroll
/// it".
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

/// The shape that actually vectorizes: `chunks_exact` + `try_into` gives LLVM
/// a `&[f32; 8]` whose length is a compile-time constant, so the bounds checks
/// vanish and the eight lanes fold into one vector register.
///
/// Still computes all three quantities, so it still carries 24 live lanes.
/// Measured against [`dot_unit_f32`] this is the "vectorizable loop, too much
/// register pressure to stay vectorized" data point.
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

/// L2-normalize once, so the O(n²) inner loop is a bare dot product.
///
/// The current pairwise pass recomputes `‖a‖` and `‖b‖` inside every one of
/// the n(n-1)/2 comparisons even though each vector has exactly one norm —
/// two thirds of the arithmetic is redundant. Hoisting it to an O(n) prepass
/// is the algorithmic half of the win; it is also what drops the inner loop
/// to a single 8-lane accumulator set, which is what lets it stay in vector
/// registers.
fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Dot product of two already-normalized vectors == their cosine similarity.
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
    // --- single pair: isolates the vectorization, no threading involved ---
    {
        let mut group = c.benchmark_group("cosine_pair");
        let a = synth_vector(1);
        let b_vec = synth_vector(2);

        let a_unit = l2_normalize(&a);
        let b_unit = l2_normalize(&b_vec);

        group.bench_function("f64_scalar", |b| b.iter(|| cosine_f64_scalar(&a, &b_vec)));
        group.bench_function("f32_scalar", |b| b.iter(|| cosine_f32_scalar(&a, &b_vec)));
        group.bench_function("f32_lanes8", |b| b.iter(|| cosine_f32_lanes8(&a, &b_vec)));
        group.bench_function("f32_arr8", |b| b.iter(|| cosine_f32_arr8(&a, &b_vec)));
        group.bench_function("dot_unit", |b| b.iter(|| dot_unit_f32(&a_unit, &b_unit)));
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

        // The candidate: O(n) normalize prepass, then a vectorized dot in the
        // O(n^2) body.
        group.bench_with_input(BenchmarkId::new("norm_dot", n), &vectors, |b, v| {
            b.iter(|| {
                let unit: Vec<Vec<f32>> = v.iter().map(|x| l2_normalize(x)).collect();
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

        // …and the same with the outer loop spread over the pool. Row `i`
        // does `n - i` comparisons, so the work per index is triangular:
        // rayon's work-stealing handles the imbalance without manual chunking.
        group.bench_with_input(BenchmarkId::new("norm_dot_rayon", n), &vectors, |b, v| {
            b.iter(|| {
                let unit: Vec<Vec<f32>> = v.par_iter().map(|x| l2_normalize(x)).collect();
                let pairs: Vec<(usize, usize)> = (0..unit.len())
                    .into_par_iter()
                    .flat_map_iter(|i| {
                        let unit = &unit;
                        ((i + 1)..unit.len()).filter_map(move |j| {
                            (dot_unit_f32(&unit[i], &unit[j]) >= threshold).then_some((i, j))
                        })
                    })
                    .collect();
                pairs.len()
            });
        });
    }

    group.finish();
}

// ===========================================================================
// Group 5: end-to-end shape of a reindex batch (parse + stems together)
// ===========================================================================

/// `reindex_dir` does both of the CPU-bound steps above back to back: parse
/// every file, then derive stems for every parsed memory inside
/// `IndexEntry::from`. This measures them fused, which is what the real path
/// would parallelize as one pass.
fn reindex_cpu_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("reindex_cpu");
    group.sample_size(10);

    for &n in SIZES {
        let files: Vec<String> = memories(n)
            .iter()
            .map(|m| write_memory_file(m).expect("serialize"))
            .collect();

        group.bench_with_input(BenchmarkId::new("serial", n), &files, |b, files| {
            b.iter(|| {
                let mut out: HashMap<String, KeywordStems> = HashMap::new();
                for content in files {
                    if let Ok(m) = parse_memory_file(content) {
                        out.insert(
                            m.id.clone(),
                            KeywordStems::compute(&m.summary, &m.tags, &m.content),
                        );
                    }
                }
                out.len()
            });
        });

        group.bench_with_input(BenchmarkId::new("rayon", n), &files, |b, files| {
            b.iter(|| {
                let out: HashMap<String, KeywordStems> = files
                    .par_iter()
                    .filter_map(|content| {
                        let m = parse_memory_file(content).ok()?;
                        Some((
                            m.id.clone(),
                            KeywordStems::compute(&m.summary, &m.tags, &m.content),
                        ))
                    })
                    .collect();
                out.len()
            });
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
);
criterion_main!(benches);
