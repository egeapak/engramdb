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
    let step = n * 4;
    let mut acc = [S::f32s::splat(simd, 0.0); 4];
    let (mut ca, mut cb) = (a.chunks_exact(step), b.chunks_exact(step));
    for (x, y) in (&mut ca).zip(&mut cb) {
        for k in 0..4 {
            acc[k] = S::f32s::from_slice(simd, &x[k * n..(k + 1) * n])
                .mul_add(S::f32s::from_slice(simd, &y[k * n..(k + 1) * n]), acc[k]);
        }
    }
    let mut d: f32 = ((acc[0] + acc[1]) + (acc[2] + acc[3]))
        .as_slice()
        .iter()
        .sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
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
