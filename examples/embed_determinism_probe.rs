//! Diagnostic: how reproducible is a single provider's output for one text?
//!
//! Written to chase an intermittent failure of
//! `embeddings::onnx::tests::test_embed_consistency` — two back-to-back
//! `embed("hello")` calls on the same provider disagreeing by up to 0.044 per
//! element on an L2-normalized vector, which is far too large to be
//! thread-scheduling round-off.
//!
//! Reports, per model, the max element-wise spread across N repeats of the same
//! text (single-call path and batch path), optionally under synthetic CPU load.
//!
//! Run: `cargo run --release --example embed_determinism_probe`
//! Env: `PROBE_ITERS` (default 30), `PROBE_LOAD_THREADS` (default 0).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use engramdb::embeddings::{
    EmbeddingProvider, OnnxModelSpec, OnnxProvider, ONNX_ALL_MINILM, ONNX_ALL_MINILM_L12_Q,
    ONNX_ALL_MINILM_Q,
};

const TEXT: &str = "hello";

fn spread(vectors: &[Vec<f32>]) -> f32 {
    let mut worst = 0.0f32;
    for i in 0..vectors[0].len() {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for v in vectors {
            lo = lo.min(v[i]);
            hi = hi.max(v[i]);
        }
        worst = worst.max(hi - lo);
    }
    worst
}

async fn probe(name: &str, spec: OnnxModelSpec, iters: usize) -> Result<()> {
    let Ok(provider) = OnnxProvider::with_model(spec) else {
        println!("{name:<16} unavailable");
        return Ok(());
    };
    let mut singles = Vec::new();
    let mut batched = Vec::new();
    for _ in 0..iters {
        singles.push(provider.embed(TEXT).await?);
        batched.push(provider.embed_batch(&[TEXT]).await?.remove(0));
    }
    // Cross-path: is embed() the same as a 1-element embed_batch()?
    let mut both = singles.clone();
    both.extend(batched.iter().cloned());
    println!(
        "{name:<16} max element spread — embed(): {:.6}  embed_batch(): {:.6}  both: {:.6}  \
         | min pairwise cosine: {:.6}",
        spread(&singles),
        spread(&batched),
        spread(&both),
        min_pairwise_cosine(&both),
    );
    Ok(())
}

/// The retrieval-relevant question: do the repeats still point the same way?
/// Element-wise spread can look alarming while cosine stays ~1.
fn min_pairwise_cosine(vectors: &[Vec<f32>]) -> f32 {
    let mut worst = 1.0f32;
    for (i, a) in vectors.iter().enumerate() {
        for b in &vectors[i + 1..] {
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            if na > 0.0 && nb > 0.0 {
                worst = worst.min(dot / (na * nb));
            }
        }
    }
    worst
}

#[tokio::main]
async fn main() -> Result<()> {
    let iters: usize = std::env::var("PROBE_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let load: usize = std::env::var("PROBE_LOAD_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..load {
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(Ordering::Relaxed) {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                std::hint::black_box(x);
            }
        }));
    }

    println!("max element-wise spread over {iters} repeats of {TEXT:?} (load threads: {load})");
    probe("minilm-l6-q", ONNX_ALL_MINILM_Q, iters).await?;
    probe("minilm-l12-q", ONNX_ALL_MINILM_L12_Q, iters).await?;
    probe("minilm-l6-fp32", ONNX_ALL_MINILM, iters).await?;

    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}
