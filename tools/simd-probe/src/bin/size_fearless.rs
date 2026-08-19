//! Size case: the same dot product via fearless_simd, with exactly **one**
//! `dispatch!` site, and the same four-accumulator body that ships — so this is
//! what `ops::compress::dot_unit` actually costs.
//!
//! The main probe binary has four dispatch sites and so overstates this: each
//! one monomorphises the kernel per SIMD level and drags in that level's
//! `vectorize` wrapper. Compare against `size_scalar`.

use fearless_simd::*;

#[inline(always)]
fn dot_inner<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
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

fn dot(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    dispatch!(Level::new(), simd => dot_inner(simd, a, b))
}

fn main() {
    let v: Vec<f32> = std::env::args()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    let (a, b) = v.split_at(v.len() / 2);
    println!("{}", dot(a, b));
}
