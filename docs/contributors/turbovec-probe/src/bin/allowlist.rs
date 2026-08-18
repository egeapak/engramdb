//! Does turbovec's allowlist actually reduce work proportionally (a prefilter),
//! or is it a post-gate that costs the full scan regardless of selectivity?
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

fn main() {
    println!("allowlist selectivity vs query time (d=384, 4-bit, k=10)");
    println!("a real prefilter should get FASTER as the allowed fraction shrinks\n");
    println!(
        "{:>7} | {:>10} | {:>12} | {:>12} | {:>12} | {:>12}",
        "n", "no filter", "100% allowed", "50%", "10%", "1%"
    );
    println!("{}", "-".repeat(80));

    for &n in &[2000usize, 10_000, 50_000] {
        let data = corpus(n, 1);
        let q = corpus(1, 9);
        let ids: Vec<u64> = (0..n as u64).collect();
        let mut idx = IdMapIndex::new(D, 4).unwrap();
        idx.add_with_ids_2d(&data, D, &ids).unwrap();
        idx.prepare();

        let reps = 200u32;
        for _ in 0..50 {
            let _ = idx.search(&q, 10);
        }
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(idx.search(&q, 10));
        }
        let base = t.elapsed() / reps;

        let mut cells = vec![];
        for &frac in &[100usize, 50, 10, 1] {
            let allowed: Vec<u64> = ids
                .iter()
                .copied()
                .filter(|i| (*i as usize) % 100 < frac)
                .collect();
            for _ in 0..20 {
                let _ = idx.search_with_allowlist(&q, 10, Some(&allowed));
            }
            let t = Instant::now();
            for _ in 0..reps {
                std::hint::black_box(idx.search_with_allowlist(&q, 10, Some(&allowed)).unwrap());
            }
            cells.push(t.elapsed() / reps);
        }
        println!(
            "{:>7} | {:>10.2?} | {:>12.2?} | {:>12.2?} | {:>12.2?} | {:>12.2?}",
            n, base, cells[0], cells[1], cells[2], cells[3]
        );
    }

    // Does an id absent from the index fail the whole query?
    println!("\n--- unknown id in allowlist ---");
    let data = corpus(100, 1);
    let ids: Vec<u64> = (0..100u64).collect();
    let mut idx = IdMapIndex::new(D, 4).unwrap();
    idx.add_with_ids_2d(&data, D, &ids).unwrap();
    let q = corpus(1, 9);
    match idx.search_with_allowlist(&q, 10, Some(&[1, 2, 99999])) {
        Ok((_, got)) => println!("  Ok, returned {} ids -> tolerant", got.len()),
        Err(e) => println!("  Err({e:?}) -> the ENTIRE query fails on one unknown id"),
    }
}
