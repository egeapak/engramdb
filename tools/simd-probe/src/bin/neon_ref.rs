//! The deleted `dot_unit_neon` backend, verbatim, next to the shipped
//! `fearless_simd` kernel — so the aarch64 comparison this branch could not
//! run on hardware can at least be made statically.
//!
//! Prints both results (correctness) and the two hot loops are extracted from
//! the disassembly by `aarch64.sh` for `llvm-mca`.
//! Builds on any target so `cargo build --bins` and `clippy --all-targets`
//! stay green; the body is aarch64-only and `main` says so elsewhere.

#[cfg(not(target_arch = "aarch64"))]
fn main() {
    eprintln!("neon_ref is aarch64-only; run it via ./aarch64.sh (cross + QEMU).");
}

#[cfg(target_arch = "aarch64")]
use fearless_simd::*;

/// Shipped kernel: `ops::compress::dot_unit_kernel`, verbatim.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn fs_inner<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
    let n = S::f32s::N;
    let mut acc = [S::f32s::splat(simd, 0.0); 4];
    let (mut wide_a, mut wide_b) = (a.chunks_exact(n * 4), b.chunks_exact(n * 4));
    for (x, y) in (&mut wide_a).zip(&mut wide_b) {
        for k in 0..4 {
            acc[k] = S::f32s::from_slice(simd, &x[k * n..(k + 1) * n])
                .mul_add(S::f32s::from_slice(simd, &y[k * n..(k + 1) * n]), acc[k]);
        }
    }
    let (mut tail_a, mut tail_b) = (
        wide_a.remainder().chunks_exact(n),
        wide_b.remainder().chunks_exact(n),
    );
    for (x, y) in (&mut tail_a).zip(&mut tail_b) {
        acc[0] = S::f32s::from_slice(simd, x).mul_add(S::f32s::from_slice(simd, y), acc[0]);
    }
    let mut d: f32 = ((acc[0] + acc[1]) + (acc[2] + acc[3]))
        .as_slice()
        .iter()
        .sum();
    for (x, y) in tail_a.remainder().iter().zip(tail_b.remainder()) {
        d += x * y;
    }
    d as f64
}

#[cfg(target_arch = "aarch64")]
#[inline(never)]
pub fn dot_fearless(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    dispatch!(Level::new(), simd => fs_inner(simd, a, b))
}

/// The deleted `dot_unit_neon`, verbatim: 8 elements per iteration, two
/// accumulators, `vfmaq_f32`.
///
/// # Safety
/// `target_arch = "aarch64"` guarantees these instructions exist.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
pub unsafe fn dot_neon(a: &[f32], b: &[f32]) -> f64 {
    use std::arch::aarch64::*;
    let (mut acc0, mut acc1) = (vdupq_n_f32(0.0), vdupq_n_f32(0.0));
    let n = a.len() / 8 * 8;
    let mut i = 0;
    while i < n {
        acc0 = vfmaq_f32(
            acc0,
            vld1q_f32(a.as_ptr().add(i)),
            vld1q_f32(b.as_ptr().add(i)),
        );
        acc1 = vfmaq_f32(
            acc1,
            vld1q_f32(a.as_ptr().add(i + 4)),
            vld1q_f32(b.as_ptr().add(i + 4)),
        );
        i += 8;
    }
    let mut dot = vaddvq_f32(vaddq_f32(acc0, acc1));
    while i < a.len() {
        dot += a[i] * b[i];
        i += 1;
    }
    dot as f64
}

#[cfg(target_arch = "aarch64")]
#[inline(never)]
fn dot_scalar(a: &[f32], b: &[f32]) -> f64 {
    let mut d = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        d += x * y;
    }
    d as f64
}

#[cfg(target_arch = "aarch64")]
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

#[cfg(target_arch = "aarch64")]
fn main() {
    println!("level: {:?}", Level::new());
    let mut worst = 0.0f64;
    // Straddles N (4), the mop-up loop, and the 4*N=16 wide stride.
    for dims in [
        1, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 47, 63, 64, 65, 127, 128, 129, 384, 385, 768,
    ] {
        for seed in 0..8u64 {
            let a = synth(seed * 2, dims);
            let b = synth(seed * 2 + 1, dims);
            let want = dot_scalar(&a, &b);
            let fs = dot_fearless(&a, &b);
            // SAFETY: aarch64 guarantees NEON.
            let neon = unsafe { dot_neon(&a, &b) };
            for (name, got) in [("fearless", fs), ("neon", neon)] {
                let err = (got - want).abs();
                assert!(
                    err < 1e-5,
                    "{name} dims={dims} seed={seed}: {got} vs {want}"
                );
                if err > worst {
                    worst = err;
                }
            }
        }
    }
    println!("fearless_simd and the deleted NEON backend both agree with scalar");
    println!("worst absolute error: {worst:.3e}");
}
