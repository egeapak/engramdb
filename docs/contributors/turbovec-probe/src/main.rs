use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::time::Instant;
use turbovec::IdMapIndex;

const D: usize = 384;

/// Gaussian unit vectors, optionally anisotropic: real sentence-embedding
/// spaces are far from isotropic, and TurboQuant's Beta assumption is what
/// anisotropy breaks. `aniso` scales coordinate i by a power-law factor.
fn corpus(n: usize, seed: u64, aniso: f32) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut w = vec![1.0f32; D];
    if aniso > 0.0 {
        for (i, wi) in w.iter_mut().enumerate() {
            *wi = ((i + 1) as f32).powf(-aniso);
        }
    }
    let mut out = Vec::with_capacity(n * D);
    for _ in 0..n {
        let mut v: Vec<f32> = (0..D)
            .map(|i| {
                let (u1, u2): (f32, f32) = (rng.gen_range(1e-7..1.0), rng.gen_range(0.0..1.0));
                (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos() * w[i]
            })
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= norm;
        }
        out.extend_from_slice(&v);
    }
    out
}

fn exact_topk(data: &[f32], n: usize, q: &[f32], k: usize) -> Vec<u64> {
    let mut s: Vec<(f32, u64)> = (0..n)
        .map(|i| {
            let dot: f32 = (0..D).map(|j| data[i * D + j] * q[j]).sum();
            (dot, i as u64)
        })
        .collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    s.truncate(k);
    s.into_iter().map(|(_, i)| i).collect()
}

fn run(n: usize, bits: usize, calibrate: bool, aniso: f32) {
    let data = corpus(n, 42, aniso);
    let queries = corpus(50, 7, aniso);
    let ids: Vec<u64> = (0..n as u64).collect();

    let mut idx = IdMapIndex::new(D, bits).unwrap();
    if calibrate {
        let sample = &data[..(1024.min(n)) * D];
        idx.calibrate_2d(sample, D).unwrap();
    }
    let t = Instant::now();
    idx.add_with_ids_2d(&data, D, &ids).unwrap();
    let build = t.elapsed();
    idx.prepare();

    // recall@10 vs exact float32 cosine
    let k = 10;
    let mut hits = 0usize;
    let mut total = 0usize;
    let mut top1 = 0usize;
    let mut score_err = 0.0f64;
    for qi in 0..50 {
        let q = &queries[qi * D..(qi + 1) * D];
        let truth = exact_topk(&data, n, q, k);
        let (scores, got) = idx.search(q, k);
        if !got.is_empty() && got[0] == truth[0] {
            top1 += 1;
        }
        for (rank, id) in got.iter().enumerate() {
            if truth.contains(id) {
                hits += 1;
            }
            if rank == 0 {
                let exact: f32 = (0..D).map(|j| data[*id as usize * D + j] * q[j]).sum();
                score_err += (scores[0] - exact).abs() as f64;
            }
        }
        total += truth.len();
    }

    // query latency
    let q = &queries[..D];
    for _ in 0..20 {
        let _ = idx.search(q, k);
    }
    let t = Instant::now();
    let reps = 200;
    for _ in 0..reps {
        let _ = idx.search(q, k);
    }
    let per_query = t.elapsed() / reps;

    let f32_bytes = n * D * 4;
    let tv_bytes = idx.to_bytes().len();

    println!(
        "n={:>6} bits={} calib={:<5} aniso={:<4} | recall@10={:.3} top1={:.2} scoreErr={:.4} | build={:>9.2?} query={:>9.2?} | f32={:>8} tv={:>8} ({:.1}x)",
        n, bits, calibrate, aniso,
        hits as f64 / total as f64,
        top1 as f64 / 50.0,
        score_err / 50.0,
        build, per_query,
        f32_bytes, tv_bytes,
        f32_bytes as f64 / tv_bytes as f64
    );
}

fn main() {
    println!("=== d=384, recall@10 vs exact f32 inner product, 50 queries ===\n");
    for &aniso in &[0.0f32, 0.5] {
        println!(
            "--- anisotropy exponent {aniso} ({}) ---",
            if aniso == 0.0 {
                "isotropic: TurboQuant's best case"
            } else {
                "anisotropic: closer to real embeddings"
            }
        );
        for &n in &[500usize, 2_000, 10_000, 100_000] {
            for &bits in &[2usize, 4] {
                for &cal in &[false, true] {
                    run(n, bits, cal, aniso);
                }
            }
        }
        println!();
    }
}
