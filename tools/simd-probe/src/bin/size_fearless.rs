//! Size case: the same dot product via fearless_simd, with exactly **one**
//! `dispatch!` site — what an adoption inside `dot_unit` would actually cost.
//!
//! The main probe binary has four dispatch sites and so overstates this: each
//! one monomorphises the kernel per SIMD level and drags in that level's
//! `vectorize` wrapper. Compare against `size_scalar`.

use fearless_simd::*;

#[inline(always)]
fn dot_inner<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f64 {
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
    let mut d: f32 = (acc0 + acc1).as_slice().iter().sum();
    while i < a.len() {
        d += a[i] * b[i];
        i += 1;
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
