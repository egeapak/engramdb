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
    ONNX_ALL_MINILM_L12_U8, ONNX_ALL_MINILM_Q,
};

/// Inputs of increasing length: a single token, a short phrase, and a
/// realistic memory-sized chunk. If reproducibility depends on sequence
/// length, this shows it.
const TEXTS: &[(&str, &str)] = &[
    ("1tok", "hello"),
    ("short", "the retrieval engine caches providers per process"),
    (
        "long",
        "We chose LanceDB over Qdrant for the vector store because it embeds \
         in-process with zero external services, supports the Arrow columnar \
         format we already use for the metadata table, and lets a single table \
         hold both the filterable metadata and the embedding vectors. The \
         alternative would have meant running a separate service per project, \
         which is unacceptable for a tool that has to start instantly inside a \
         coding agent session and leave no daemon behind when it exits.",
    ),
];

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

async fn probe(name: &str, spec: OnnxModelSpec, iters: usize, intra: usize) -> Result<()> {
    let Ok(provider) =
        OnnxProvider::with_model_on_intra(spec, engramdb::onnx_ep::Backend::Cpu, Some(intra))
    else {
        println!("{name:<16} unavailable");
        return Ok(());
    };
    for (label, text) in TEXTS {
        let mut singles = Vec::new();
        let mut batched = Vec::new();
        for _ in 0..iters {
            singles.push(provider.embed(text).await?);
            batched.push(provider.embed_batch(&[text]).await?.remove(0));
        }
        // Cross-path: is embed() the same as a 1-element embed_batch()?
        let mut both = singles.clone();
        both.extend(batched.iter().cloned());
        println!(
            "  {name:<16} {label:<6} distinct: {:>2}/{:<3} | max spread {:.6} \
             | min cosine {:.6}",
            distinct(&both),
            both.len(),
            spread(&both),
            min_pairwise_cosine(&both),
        );
    }
    Ok(())
}

/// How many *different* vectors the repeats produced. 1 = fully reproducible.
/// A small number with a large spread means rare outliers, not drift.
fn distinct(vectors: &[Vec<f32>]) -> usize {
    let mut seen: Vec<&Vec<f32>> = Vec::new();
    for v in vectors {
        if !seen.iter().any(|s| {
            s.iter()
                .zip(v.iter())
                .all(|(a, b)| (a - b).abs() < f32::EPSILON)
        }) {
            seen.push(v);
        }
    }
    seen.len()
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

    println!(
        "{iters} repeats x {} texts, per (model, intra_threads). \
         background load threads: {load}",
        TEXTS.len()
    );
    // One model per process is the point of PROBE_MODELS: loading several ORT
    // sessions in one process is itself a source of contention, so a clean
    // single-model reading needs the others out of the way.
    let filter: Option<Vec<String>> = std::env::var("PROBE_MODELS")
        .ok()
        .map(|s| s.split(',').map(|m| m.trim().to_string()).collect());
    let want = |k: &str| filter.as_ref().is_none_or(|f| f.iter().any(|m| m == k));
    let intras: Vec<usize> = std::env::var("PROBE_INTRA")
        .ok()
        .map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 2, 4]);
    for intra in intras {
        println!("-- intra_threads = {intra} --");
        if want("minilm-l6-q") {
            probe("minilm-l6-q", ONNX_ALL_MINILM_Q, iters, intra).await?;
        }
        if want("minilm-l12-q") {
            probe("minilm-l12-q", ONNX_ALL_MINILM_L12_Q, iters, intra).await?;
        }
        if want("minilm-l12-u8") {
            probe("minilm-l12-u8", ONNX_ALL_MINILM_L12_U8, iters, intra).await?;
        }
        if want("minilm-l6-fp32") {
            probe("minilm-l6-fp32", ONNX_ALL_MINILM, iters, intra).await?;
        }
    }

    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}
