//! The harness behind `ops::compress::dot_unit`'s SIMD choice.
//!
//! It compares the shipped `fearless_simd` kernel against the four hand-written
//! `std::arch` backends it replaced, at the profile EngramDB actually builds
//! with. Both are kept here permanently: the intrinsics are the baseline the
//! crate has to keep beating, and the day it stops, this is where that shows.
//!
//! This is also what caught the two mistakes that made the first evaluation
//! reject the crate outright — the wrong accumulator count and the wrong
//! `opt-level`. Run it before changing the kernel, not after.
//!
//! ```text
//! cd tools/simd-probe
//! cargo run --release     # mirrors [profile.release]: opt-level 2, fat LTO
//! cargo run --profile oz    # what the old -Oz profile did
//! cargo run --profile o3    # is a result opt-level-specific?
//! ./size.sh               # the size axis
//! ```
//!
//! `dispatch!` always normalises *up* to the best level the CPU has
//! (`Level::__dispatch_target`), so a lower level cannot be selected at run
//! time — it has to be excluded at build time. Every dev box and CI runner has
//! AVX2 or better, so this is the only way to exercise what a pre-Haswell CPU
//! or an AVX2-masking VM will run:
//!
//! ```text
//! RUSTFLAGS='--cfg disable_dispatch_avx512' cargo run --release
//! RUSTFLAGS='--cfg disable_dispatch_avx512 --cfg disable_dispatch_avx2' cargo run --release
//! RUSTFLAGS='--cfg disable_dispatch_avx512 --cfg disable_dispatch_avx2 --cfg disable_dispatch_sse4_2' cargo run --release
//! ```
//!
//! (The `simd-levels` CI job runs the *correctness* tests that way. This probe
//! is for performance; correctness lives in `ops::compress::tests`.)
//!
//! Two methodology notes, both learned the hard way:
//!
//! 1. **Candidates are interleaved**, round-robin inside one process, and
//!    scored on their per-candidate minimum. Measuring them sequentially gave
//!    a 70% swing between invocations of the same binary — a frequency dip
//!    landed entirely on whichever candidate was running. Absolute ns/pair on
//!    a shared host still drifts by 2x between sessions, so compare *within* a
//!    run and treat cross-session absolutes as meaningless.
//! 2. **Do not force a level with `Token::assume_supported()`.** It yields a
//!    proof token but does *not* enable the target feature on the calling
//!    function, so the monomorphised body compiles for the baseline and
//!    measures emulation — that route made AVX2 look ~12x slower than the
//!    same kernel reached through `dispatch!`. Use `--cfg disable_dispatch_*`
//!    instead; it is the crate's own supported knob.

use fearless_simd::*;
use std::hint::black_box;
use std::time::Instant;

/// 384 is the all-MiniLM embedding width, i.e. what `consolidation_pass`
/// actually compares.
const DIM: usize = 384;

// ---------------------------------------------------------------- scalar ---

/// The portable fallback in `ops::compress`, verbatim: one accumulator.
#[inline(never)]
fn dot_scalar(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
    }
    dot as f64
}

// --------------------------------------- what ships today, copied verbatim ---

#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn dot_sse2(a: &[f32], b: &[f32]) -> f64 {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is unconditionally present on x86_64; `n` is `len` rounded
    // down to a multiple of 8, so every lane of every load is in bounds.
    unsafe {
        let (mut acc0, mut acc1) = (_mm_setzero_ps(), _mm_setzero_ps());
        let n = a.len() / 8 * 8;
        let mut i = 0;
        while i < n {
            let a0 = _mm_loadu_ps(a.as_ptr().add(i));
            let b0 = _mm_loadu_ps(b.as_ptr().add(i));
            let a1 = _mm_loadu_ps(a.as_ptr().add(i + 4));
            let b1 = _mm_loadu_ps(b.as_ptr().add(i + 4));
            acc0 = _mm_add_ps(acc0, _mm_mul_ps(a0, b0));
            acc1 = _mm_add_ps(acc1, _mm_mul_ps(a1, b1));
            i += 8;
        }
        let mut lanes = [0.0f32; 4];
        _mm_storeu_ps(lanes.as_mut_ptr(), _mm_add_ps(acc0, acc1));
        let mut dot: f32 = lanes.iter().sum();
        while i < a.len() {
            dot += a[i] * b[i];
            i += 1;
        }
        dot as f64
    }
}

/// # Safety
/// The caller must have verified `avx2` and `fma` are available.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[inline(never)]
unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f64 {
    use std::arch::x86_64::*;
    let (mut acc0, mut acc1) = (_mm256_setzero_ps(), _mm256_setzero_ps());
    let n = a.len() / 16 * 16;
    let mut i = 0;
    while i < n {
        let a0 = _mm256_loadu_ps(a.as_ptr().add(i));
        let b0 = _mm256_loadu_ps(b.as_ptr().add(i));
        let a1 = _mm256_loadu_ps(a.as_ptr().add(i + 8));
        let b1 = _mm256_loadu_ps(b.as_ptr().add(i + 8));
        acc0 = _mm256_fmadd_ps(a0, b0, acc0);
        acc1 = _mm256_fmadd_ps(a1, b1, acc1);
        i += 16;
    }
    let mut lanes = [0.0f32; 8];
    _mm256_storeu_ps(lanes.as_mut_ptr(), _mm256_add_ps(acc0, acc1));
    let mut dot: f32 = lanes.iter().sum();
    while i < a.len() {
        dot += a[i] * b[i];
        i += 1;
    }
    dot as f64
}

/// `dot_unit`'s dispatch, verbatim — the number every candidate is scored on.
#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn dot_current(a: &[f32], b: &[f32]) -> f64 {
    if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
        // SAFETY: guarded by the detection immediately above.
        unsafe { dot_avx2(a, b) }
    } else {
        dot_sse2(a, b)
    }
}

// ----------------------------------------------------------- fearless_simd ---
//
// The `dot_fs*` kernels below index manually (`&a[i..i + w]`). The crate's own
// examples (`sigmoid.rs`, `disable_avx2_for_one_function.rs`) instead zip two
// `chunks_exact` iterators, which is the documented idiom; `dot_idio*` are that
// form. Both are kept so the difference is measurable rather than assumed.

/// The documented idiom, one accumulator: zip two `chunks_exact(S::f32s::N)`.
#[inline(always)]
fn dot_idio1<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let n = S::f32s::N;
    let mut acc = S::f32s::splat(simd, 0.0);
    let (mut ca, mut cb) = (a.chunks_exact(n), b.chunks_exact(n));
    for (x, y) in (&mut ca).zip(&mut cb) {
        acc = S::f32s::from_slice(simd, x).mul_add(S::f32s::from_slice(simd, y), acc);
    }
    let mut dot: f32 = acc.as_slice().iter().sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        dot += x * y;
    }
    dot as f64
}

/// The documented idiom, two independent accumulators — matching the shape the
/// hand-written backends use, so the comparison stays like-for-like.
#[inline(always)]
fn dot_idio2<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let n = S::f32s::N;
    let step = n * 2;
    let mut acc0 = S::f32s::splat(simd, 0.0);
    let mut acc1 = S::f32s::splat(simd, 0.0);
    let (mut ca, mut cb) = (a.chunks_exact(step), b.chunks_exact(step));
    for (x, y) in (&mut ca).zip(&mut cb) {
        let (x0, x1) = x.split_at(n);
        let (y0, y1) = y.split_at(n);
        acc0 = S::f32s::from_slice(simd, x0).mul_add(S::f32s::from_slice(simd, y0), acc0);
        acc1 = S::f32s::from_slice(simd, x1).mul_add(S::f32s::from_slice(simd, y1), acc1);
    }
    let mut dot: f32 = (acc0 + acc1).as_slice().iter().sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        dot += x * y;
    }
    dot as f64
}

/// Four accumulators — more independent FMA chains to hide latency.
#[inline(always)]
fn dot_idio4<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let n = S::f32s::N;
    let step = n * 4;
    let mut acc = [S::f32s::splat(simd, 0.0); 4];
    let (mut ca, mut cb) = (a.chunks_exact(step), b.chunks_exact(step));
    for (x, y) in (&mut ca).zip(&mut cb) {
        for k in 0..4 {
            acc[k] = S::f32s::from_slice(simd, &x[k * n..(k + 1) * n])
                .mul_add(S::f32s::from_slice(simd, &y[k * n..(k + 1) * n]), acc[k]);
        }
    }
    let mut dot: f32 = ((acc[0] + acc[1]) + (acc[2] + acc[3]))
        .as_slice()
        .iter()
        .sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        dot += x * y;
    }
    dot as f64
}

/// `dot_idio2`, but never compiled for AVX-512.
///
/// Straight out of the crate's `disable_avx2_for_one_function.rs`, which exists
/// for exactly this case: "useful if benchmarks show a specific instruction set
/// regressing performance". Measured here, AVX-512 buys nothing — 0.19 ns per
/// float, identical to AVX2 at half the width — because 512-bit ops trigger
/// frequency licensing on this part. The downgrade only has to happen once
/// anywhere in the call chain.
#[inline(always)]
fn dot_no512<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let level = simd.level();
        if let Level::Avx512(_) = level {
            return dot_idio2(level.as_avx2().unwrap(), a, b);
        }
    }
    dot_idio2(simd, a, b)
}

/// One accumulator at the CPU's native width.
#[inline(always)]
fn dot_fs1<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let w = S::f32s::N;
    let mut acc = S::f32s::splat(simd, 0.0);
    let n = a.len() / w * w;
    let mut i = 0;
    while i < n {
        let av = S::f32s::from_slice(simd, &a[i..i + w]);
        let bv = S::f32s::from_slice(simd, &b[i..i + w]);
        acc = av.mul_add(bv, acc);
        i += w;
    }
    let mut dot: f32 = acc.as_slice().iter().sum();
    while i < a.len() {
        dot += a[i] * b[i];
        i += 1;
    }
    dot as f64
}

/// Two independent native-width accumulators — the shape both hand-written
/// backends use, so this is the like-for-like comparison.
#[inline(always)]
fn dot_fs2<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let w = S::f32s::N;
    let step = w * 2;
    let mut acc0 = S::f32s::splat(simd, 0.0);
    let mut acc1 = S::f32s::splat(simd, 0.0);
    let n = a.len() / step * step;
    let mut i = 0;
    while i < n {
        let a0 = S::f32s::from_slice(simd, &a[i..i + w]);
        let b0 = S::f32s::from_slice(simd, &b[i..i + w]);
        let a1 = S::f32s::from_slice(simd, &a[i + w..i + step]);
        let b1 = S::f32s::from_slice(simd, &b[i + w..i + step]);
        acc0 = a0.mul_add(b0, acc0);
        acc1 = a1.mul_add(b1, acc1);
        i += step;
    }
    let mut dot: f32 = (acc0 + acc1).as_slice().iter().sum();
    while i < a.len() {
        dot += a[i] * b[i];
        i += 1;
    }
    dot as f64
}

/// Fixed 8-wide over `chunks_exact`, feeding `load_array_ref` a
/// compile-time-length `&[f32; 8]`.
///
/// The motivation was the four bounds-check branches per iteration that
/// `from_slice`'s `try_into().unwrap()` leaves in the `-Oz` disassembly — the
/// same trap the scalar lane experiment hit (see the SIMD doc). It does not
/// pay off: at `-Oz` the `chunks_exact` iterator costs more than the checks it
/// removes. It is kept because "we tried the array shape" is the first thing
/// the next reader will ask.
#[inline(always)]
fn dot_fs_x8<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let mut acc0 = f32x8::splat(simd, 0.0);
    let mut acc1 = f32x8::splat(simd, 0.0);
    let mut ca = a.chunks_exact(16);
    let mut cb = b.chunks_exact(16);
    for (ka, kb) in (&mut ca).zip(&mut cb) {
        let (a0, a1) = ka.split_at(8);
        let (b0, b1) = kb.split_at(8);
        acc0 = f32x8::load_array_ref(simd, a0.try_into().unwrap())
            .mul_add(f32x8::load_array_ref(simd, b0.try_into().unwrap()), acc0);
        acc1 = f32x8::load_array_ref(simd, a1.try_into().unwrap())
            .mul_add(f32x8::load_array_ref(simd, b1.try_into().unwrap()), acc1);
    }
    let mut dot: f32 = (acc0 + acc1).as_slice().iter().sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        dot += x * y;
    }
    dot as f64
}

/// Drop-in shape: dispatch inside, once per call, exactly where
/// `is_x86_feature_detected!` sits in `dot_unit` today.
#[inline(never)]
fn dot_fs1_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_fs1(simd, a, b))
}

#[inline(never)]
fn dot_fs2_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_fs2(simd, a, b))
}

#[inline(never)]
fn dot_fs_x8_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_fs_x8(simd, a, b))
}

#[inline(never)]
fn dot_idio1_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_idio1(simd, a, b))
}

#[inline(never)]
fn dot_idio2_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_idio2(simd, a, b))
}

#[inline(never)]
fn dot_idio4_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_idio4(simd, a, b))
}

#[inline(never)]
fn dot_no512_dispatch(a: &[f32], b: &[f32]) -> f64 {
    dispatch!(Level::new(), simd => dot_no512(simd, a, b))
}

// --------------------------------------------------- whole-pass comparison ---
//
// The per-call numbers above charge fearless_simd for one `dispatch!` per pair.
// `consolidation_pass` could instead resolve the level once and run the entire
// O(n^2) body inside a single monomorphised call, which is the best case the
// crate can offer. These two functions measure that.

#[inline(never)]
fn pairwise_current(vectors: &[Vec<f32>], threshold: f64) -> usize {
    let mut hits = 0;
    for i in 0..vectors.len() {
        for j in (i + 1)..vectors.len() {
            #[cfg(target_arch = "x86_64")]
            let d = dot_current(&vectors[i], &vectors[j]);
            #[cfg(not(target_arch = "x86_64"))]
            let d = dot_scalar(&vectors[i], &vectors[j]);
            if d >= threshold {
                hits += 1;
            }
        }
    }
    hits
}

#[inline(never)]
fn pairwise_fs(vectors: &[Vec<f32>], threshold: f64) -> usize {
    dispatch!(Level::new(), simd => pairwise_fs_inner(simd, vectors, threshold))
}

#[inline(always)]
fn pairwise_fs_inner<S: Simd>(simd: S, vectors: &[Vec<f32>], threshold: f64) -> usize {
    let mut hits = 0;
    for i in 0..vectors.len() {
        for j in (i + 1)..vectors.len() {
            if dot_fs2(simd, &vectors[i], &vectors[j]) >= threshold {
                hits += 1;
            }
        }
    }
    hits
}

// ----------------------------------------------------------------- harness ---

/// Deterministic pseudo-random vectors: an LCG, so a run is reproducible and
/// no `rand` dependency is needed.
fn synth(seed: u64, len: usize) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..len)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        })
        .collect()
}

/// `dot_unit` is only a cosine for unit vectors, so feed it unit vectors.
fn l2_normalized(v: &[f32]) -> Vec<f32> {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / n).collect()
}

type Kernel = (&'static str, fn(&[f32], &[f32]) -> f64);

fn main() {
    let rounds: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let reps = 20_000;

    let a = l2_normalized(&synth(1, DIM));
    let b = l2_normalized(&synth(2, DIM));

    println!(
        "level {:?}, dim {DIM}, {rounds} interleaved rounds x {reps} reps",
        Level::new()
    );
    println!(
        "note: Level::new() reports the CPU's level; --cfg disable_dispatch_* lowers what\n      \
         dispatch! actually selects without changing this line.\n"
    );

    let mut kernels: Vec<Kernel> = vec![
        ("scalar (1 acc)", dot_scalar),
        ("fearless_simd 1 acc", dot_fs1_dispatch),
        ("fearless_simd 2 acc", dot_fs2_dispatch),
        ("fearless_simd x8 arrays", dot_fs_x8_dispatch),
        ("fs idiomatic 1 acc", dot_idio1_dispatch),
        ("fs idiomatic 2 acc", dot_idio2_dispatch),
        ("fs idiomatic 4 acc", dot_idio4_dispatch),
        ("fs idiomatic, no avx512", dot_no512_dispatch),
    ];
    #[cfg(target_arch = "x86_64")]
    {
        kernels.push(("intrinsics sse2", dot_sse2));
        // SAFETY: only reached when the detection below says avx2+fma exist.
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            kernels.push(("intrinsics avx2+fma", |a, b| unsafe { dot_avx2(a, b) }));
        }
        kernels.push(("intrinsics dispatch (current)", dot_current));
    }

    // Correctness gate before any timing: a fast wrong answer is not a result.
    let want = dot_scalar(&a, &b);
    for (name, f) in &kernels {
        let got = f(&a, &b);
        assert!(
            (got - want).abs() < 1e-5,
            "{name} disagrees with the scalar reference: {got} vs {want}"
        );
    }
    println!("all {} kernels agree with scalar to 1e-5\n", kernels.len());

    let mut best = vec![f64::MAX; kernels.len()];
    for (_, f) in &kernels {
        for _ in 0..reps {
            black_box(f(&a, &b));
        }
    }
    for _ in 0..rounds {
        for (i, (_, f)) in kernels.iter().enumerate() {
            let t = Instant::now();
            for _ in 0..reps {
                black_box(f(&a, &b));
            }
            let ns = t.elapsed().as_nanos() as f64 / reps as f64;
            best[i] = best[i].min(ns);
        }
    }
    let baseline = kernels
        .iter()
        .position(|(n, _)| *n == "intrinsics dispatch (current)")
        .map(|i| best[i])
        .unwrap_or(best[0]);
    for (i, (name, _)) in kernels.iter().enumerate() {
        println!(
            "{name:<32} {:>7.1} ns/pair   {:>5.2}x vs current",
            best[i],
            baseline / best[i]
        );
    }

    // Whole-pass shape, dispatch hoisted out of the O(n^2) body.
    let vs: Vec<Vec<f32>> = (0..200)
        .map(|i| l2_normalized(&synth(i as u64 + 10, DIM)))
        .collect();
    let pairs = vs.len() * (vs.len() - 1) / 2;
    assert_eq!(
        pairwise_current(&vs, 0.9),
        pairwise_fs(&vs, 0.9),
        "the two pairwise passes must find the same pairs"
    );
    let (mut bc, mut bf) = (f64::MAX, f64::MAX);
    for _ in 0..9 {
        let t = Instant::now();
        black_box(pairwise_current(&vs, 0.9));
        bc = bc.min(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        black_box(pairwise_fs(&vs, 0.9));
        bf = bf.min(t.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "\npairwise, {} vectors ({pairs} pairs), interleaved",
        vs.len()
    );
    println!(
        "current  (detect per pair)       {bc:>7.2} ms  ({:>5.1} ns/pair)",
        bc * 1e6 / pairs as f64
    );
    println!(
        "fearless (dispatch hoisted)      {bf:>7.2} ms  ({:>5.1} ns/pair)   {:.2}x",
        bf * 1e6 / pairs as f64,
        bc / bf
    );
}
