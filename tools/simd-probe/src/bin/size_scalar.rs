//! Size baseline: everything the other two `size_*` binaries contain *except*
//! the SIMD. Subtracting this from them is the byte cost of each approach.
//!
//! Inputs come from argv and the result is printed, so nothing is
//! const-folded away and the kernel cannot be dead-stripped.

fn dot(a: &[f32], b: &[f32]) -> f64 {
    let mut d = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        d += x * y;
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
