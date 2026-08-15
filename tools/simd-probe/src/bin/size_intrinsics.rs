//! Size case: `ops::compress::dot_unit` as it ships — SSE2 baseline, AVX2+FMA
//! behind runtime detection. Compare against `size_scalar`.

fn dot(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
        // SAFETY: guarded by the detection immediately above.
        unsafe { avx2(a, b) }
    } else {
        sse2(a, b)
    }
}

fn sse2(a: &[f32], b: &[f32]) -> f64 {
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
        let mut d: f32 = lanes.iter().sum();
        while i < a.len() {
            d += a[i] * b[i];
            i += 1;
        }
        d as f64
    }
}

/// # Safety
/// The caller must have verified `avx2` and `fma` are available.
#[target_feature(enable = "avx2,fma")]
unsafe fn avx2(a: &[f32], b: &[f32]) -> f64 {
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
    let mut d: f32 = lanes.iter().sum();
    while i < a.len() {
        d += a[i] * b[i];
        i += 1;
    }
    d as f64
}

fn main() {
    let v: Vec<f32> = std::env::args()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    let (a, b) = v.split_at(v.len() / 2);
    println!("{}", dot(a, b));
}
