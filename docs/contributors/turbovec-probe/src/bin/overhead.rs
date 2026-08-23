use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::time::Instant;
use turbovec::IdMapIndex;

const D: usize = 384;

fn corpus(n: usize, seed: u64) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(n * D);
    for _ in 0..n {
        let mut v: Vec<f32> = (0..D)
            .map(|i| {
                let (u1, u2): (f32, f32) = (rng.gen_range(1e-7..1.0), rng.gen_range(0.0..1.0));
                (-2.0 * u1.ln()).sqrt()
                    * (2.0 * std::f32::consts::PI * u2).cos()
                    * ((i + 1) as f32).powf(-0.5)
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

#[inline]
fn brute(data: &[f32], n: usize, q: &[f32], k: usize) -> Vec<(f32, u64)> {
    let mut s: Vec<(f32, u64)> = (0..n)
        .map(|i| {
            let row = &data[i * D..i * D + D];
            let mut acc = 0.0f32;
            for j in 0..D {
                acc += row[j] * q[j];
            }
            (acc, i as u64)
        })
        .collect();
    s.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    s.truncate(k);
    s
}

fn main() {
    println!("--- fixed per-index overhead (bytes), d=384 ---");
    for &bits in &[2usize, 4] {
        let mut prev: Option<(usize, usize)> = None;
        for &n in &[0usize, 1, 10, 100, 500, 2000] {
            let mut idx = IdMapIndex::new(D, bits).unwrap();
            if n > 0 {
                let data = corpus(n, 1);
                let ids: Vec<u64> = (0..n as u64).collect();
                idx.add_with_ids_2d(&data, D, &ids).unwrap();
            }
            let b = idx.to_bytes().len();
            let per = prev.map(|(pn, pb)| (b as f64 - pb as f64) / (n - pn) as f64);
            println!("  bits={bits} n={n:<5} bytes={b:<9} raw_f32={:<9} ratio={:.2}x  marginal_bytes_per_vec={}",
                n * D * 4, if b > 0 { (n * D * 4) as f64 / b as f64 } else { 0.0 },
                per.map(|p| format!("{p:.1}")).unwrap_or("-".into()));
            prev = Some((n, b));
        }
    }

    println!("\n--- query latency: turbovec vs plain f32 brute force, d=384, k=10 ---");
    for &n in &[200usize, 500, 2000, 10_000, 50_000] {
        let data = corpus(n, 1);
        let q = corpus(1, 9);
        let ids: Vec<u64> = (0..n as u64).collect();

        let mut i4 = IdMapIndex::new(D, 4).unwrap();
        i4.add_with_ids_2d(&data, D, &ids).unwrap();
        i4.prepare();
        let mut i2 = IdMapIndex::new(D, 2).unwrap();
        i2.add_with_ids_2d(&data, D, &ids).unwrap();
        i2.prepare();

        for _ in 0..50 {
            let _ = i4.search(&q, 10);
            let _ = brute(&data, n, &q, 10);
        }
        let reps = 300;
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(i4.search(&q, 10));
        }
        let tv4 = t.elapsed() / reps;
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(i2.search(&q, 10));
        }
        let tv2 = t.elapsed() / reps;
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(brute(&data, n, &q, 10));
        }
        let bf = t.elapsed() / reps;

        println!("  n={n:<6} turbovec4={tv4:>9.2?}  turbovec2={tv2:>9.2?}  plain_f32={bf:>9.2?}  speedup_4bit={:.2}x",
            bf.as_secs_f64() / tv4.as_secs_f64());
    }
}
