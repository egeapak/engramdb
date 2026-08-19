//! Does raising the embedding dimension rescue TurboQuant's recall?
//! Sweeps d ∈ {384, 768, 1024, 1536} at the corpus sizes EngramDB actually has.
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::time::Instant;
use turbovec::IdMapIndex;

fn corpus(n: usize, d: usize, seed: u64, aniso: f32) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(n * d);
    for _ in 0..n {
        let mut v: Vec<f32> = (0..d)
            .map(|i| {
                let (u1, u2): (f32, f32) = (rng.gen_range(1e-7..1.0), rng.gen_range(0.0..1.0));
                (-2.0 * u1.ln()).sqrt()
                    * (2.0 * std::f32::consts::PI * u2).cos()
                    * ((i + 1) as f32).powf(-aniso)
            })
            .collect();
        let nrm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= nrm;
        }
        out.extend_from_slice(&v);
    }
    out
}

fn exact_topk(data: &[f32], n: usize, d: usize, q: &[f32], k: usize) -> Vec<u64> {
    let mut s: Vec<(f32, u64)> = (0..n)
        .map(|i| {
            let dot: f32 = (0..d).map(|j| data[i * d + j] * q[j]).sum();
            (dot, i as u64)
        })
        .collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    s.truncate(k);
    s.into_iter().map(|(_, i)| i).collect()
}

fn main() {
    println!("recall@10 / top-1 vs exact cosine, 4-bit TQ+, anisotropic corpus\n");
    println!(
        "{:>6} | {:>6} | {:>14} | {:>10} | {:>12} | {:>13}",
        "d", "n", "recall@10/top1", "query", "fixed bytes", "break-even n"
    );
    println!("{}", "-".repeat(80));

    for &d in &[384usize, 768, 1024, 1536] {
        // fixed per-index overhead at this d
        let empty = IdMapIndex::new(d, 4).unwrap();
        let fixed = empty.to_bytes().len();
        let marginal = d / 2 + 12; // 4-bit codes + scale + id
        let f32_per = d * 4;
        let breakeven = fixed as f64 / (f32_per - marginal) as f64;

        for &n in &[500usize, 2000] {
            let data = corpus(n, d, 42, 0.5);
            let queries = corpus(30, d, 7, 0.5);
            let ids: Vec<u64> = (0..n as u64).collect();
            let mut idx = IdMapIndex::new(d, 4).unwrap();
            idx.calibrate_2d(&data[..(1024.min(n)) * d], d).unwrap();
            idx.add_with_ids_2d(&data, d, &ids).unwrap();
            idx.prepare();

            let (mut hits, mut total, mut top1) = (0usize, 0usize, 0usize);
            for qi in 0..30 {
                let q = &queries[qi * d..(qi + 1) * d];
                let truth = exact_topk(&data, n, d, q, 10);
                let (_, got) = idx.search(q, 10);
                if !got.is_empty() && got[0] == truth[0] {
                    top1 += 1;
                }
                for id in got.iter() {
                    if truth.contains(id) {
                        hits += 1;
                    }
                }
                total += truth.len();
            }
            let q = &queries[..d];
            for _ in 0..20 {
                let _ = idx.search(q, 10);
            }
            let t = Instant::now();
            for _ in 0..200 {
                std::hint::black_box(idx.search(q, 10));
            }
            let per = t.elapsed() / 200;

            println!(
                "{:>6} | {:>6} | {:>7.3} / {:.2} | {:>10.2?} | {:>12} | {:>13.0}",
                d,
                n,
                hits as f64 / total as f64,
                top1 as f64 / 30.0,
                per,
                fixed,
                breakeven
            );
        }
    }
}
