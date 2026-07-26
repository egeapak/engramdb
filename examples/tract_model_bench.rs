//! Model comparison for the **tract** (pure-Rust / Intel-Mac) path.
//!
//! `embed_model_bench` compares models on ONNX Runtime, which is the wrong
//! engine for the platform that hurts most. Intel Macs have no prebuilt ORT, so
//! they run `tract`, which is fp32-only (the int8 exports don't load) and
//! roughly 3× slower — meaning download size and per-call latency both matter
//! more there than anywhere else.
//!
//! Reports, per fp32 candidate: whether it loads under tract at all, cached
//! weight size, cold load, warm single-embed mean/p50/p95, and batch-8 cost.
//! Pair it with `embed_matrix` for the ranking quality of the same models.
//!
//! Run: `cargo run --release --features tract --example tract_model_bench`
//! Env: `TRACT_BENCH_ITERS` (default 20), `EMBED_EVAL_DATA` (corpus).

use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{
    chunk_text, EmbeddingProvider, TractEmbeddingProvider, TractModelSpec, TRACT_ALL_MINILM,
    TRACT_ALL_MINILM_L12, TRACT_ARCTIC_XS,
};
use serde::Deserialize;

const FIXED_BUDGET_TOKENS: usize = 256;

/// fp32 candidates for the tract path. `TRACT_ALL_MINILM_L12` is today's
/// default (it follows the ONNX default's depth); the other two are the
/// same-size-class alternatives.
const CANDIDATES: &[(&str, TractModelSpec)] = &[
    ("minilm-l12-fp32", TRACT_ALL_MINILM_L12),
    ("minilm-l6-fp32", TRACT_ALL_MINILM),
    ("arctic-xs-fp32", TRACT_ARCTIC_XS),
];

#[derive(Deserialize)]
struct Dataset {
    memories: Vec<Mem>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Mem {
    summary: String,
    content: String,
}

#[derive(Deserialize)]
struct Query {
    text: String,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn cached_weight_bytes(spec: &TractModelSpec) -> Option<u64> {
    let root = engramdb::storage::paths::model_cache_dir().ok()?;
    let needle = format!("models--{}", spec.repo.replace('/', "--"));
    let file = std::path::Path::new(spec.model_file).file_name()?;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if path.file_name() == Some(file) && path.to_string_lossy().contains(&needle) {
                return Some(meta.len());
            }
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_path =
        std::env::var("EMBED_EVAL_DATA").unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let iters: usize = std::env::var("TRACT_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("reading dataset {data_path}"))?;
    let ds: Dataset = serde_json::from_str(&raw).context("parsing dataset")?;
    let mut docs: Vec<String> = ds
        .memories
        .iter()
        .flat_map(|m| chunk_text(&format!("{} {}", m.summary, m.content), FIXED_BUDGET_TOKENS))
        .collect();
    docs.sort();
    let queries: Vec<String> = ds.queries.iter().map(|q| q.text.clone()).collect();

    println!(
        "tract engine | {} chunks, {} queries, {iters} warm iters\n",
        docs.len(),
        queries.len()
    );
    println!(
        "{:<18} {:<34} {:>8} {:>9} {:>10} {:>9} {:>9} {:>11}",
        "model",
        "model_id",
        "disk_MB",
        "cold_ms",
        "warm_mean",
        "warm_p50",
        "warm_p95",
        "ms/text_b8"
    );

    let mut baseline = None;
    let mut rows = Vec::new();
    for (key, spec) in CANDIDATES {
        let disk = cached_weight_bytes(spec).map(|b| b as f64 / 1_048_576.0);
        let t0 = Instant::now();
        let provider = match TractEmbeddingProvider::with_model(*spec) {
            Ok(p) => p,
            Err(e) => {
                println!("{key:<18} FAILED TO LOAD UNDER TRACT: {e:#}");
                continue;
            }
        };
        provider.embed(&queries[0]).await?;
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;

        for q in queries.iter().take(3) {
            provider.embed(q).await?;
        }
        let mut samples = Vec::with_capacity(iters);
        for i in 0..iters {
            let t = Instant::now();
            provider.embed(&queries[i % queries.len()]).await?;
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let batches = (iters / 4).max(2);
        let mut per_text = 0.0;
        let mut n = 0usize;
        for i in 0..batches {
            let start = (i * 8) % docs.len().saturating_sub(8).max(1);
            let slice: Vec<&str> = docs[start..(start + 8).min(docs.len())]
                .iter()
                .map(|s| s.as_str())
                .collect();
            let t = Instant::now();
            provider.embed_batch(&slice).await?;
            per_text += t.elapsed().as_secs_f64() * 1000.0;
            n += slice.len();
        }

        println!(
            "{:<18} {:<34} {:>8} {:>9.1} {:>10.2} {:>9.2} {:>9.2} {:>11.2}",
            key,
            provider.model_id(),
            disk.map(|m| format!("{m:.0}"))
                .unwrap_or_else(|| "?".into()),
            cold_ms,
            mean,
            percentile(&samples, 0.50),
            percentile(&samples, 0.95),
            per_text / n.max(1) as f64,
        );
        if *key == "minilm-l12-fp32" {
            baseline = Some(mean);
        }
        rows.push((*key, mean));
    }

    if let Some(base) = baseline {
        println!("\nwarm-mean relative to the current tract default (minilm-l12-fp32):");
        for (key, mean) in &rows {
            println!("  {key:<18} {:.2}x", mean / base);
        }
    }
    Ok(())
}
