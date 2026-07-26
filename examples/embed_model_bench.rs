//! Apples-to-apples latency / footprint comparison of embedding-model candidates.
//!
//! `embed_matrix` answers "which model *ranks* best" but its `ms_per_text` is
//! an average over every variant it runs, so models with different context
//! windows are timed on different-length inputs. This harness fixes that: every
//! model embeds the **same** texts, chunked at the **same** 256-token budget,
//! so the only variable is the model.
//!
//! Reported per model:
//! - **cold**: `OnnxProvider` construction (ONNX session init) + first embed —
//!   what a CLI invocation pays when no daemon is running.
//! - **warm single**: mean / p50 / p95 over N single-text embeds — the query
//!   path (one embed per `query` call).
//! - **batch16**: ms per batch and ms per text at batch size 16 — the create /
//!   reindex path (`embed_batch` over a memory's chunks).
//! - **disk**: size of the cached ONNX weights, i.e. what the daemon holds
//!   resident and what a first run downloads.
//!
//! Run: `cargo run --release --example embed_model_bench`
//! Env: `EMBED_BENCH_MODELS` (comma filter, e.g. "minilm-q,arctic-xs-q"),
//!      `EMBED_BENCH_ITERS` (warm iterations, default 60),
//!      `EMBED_EVAL_DATA` (corpus, default `examples/data/embed_eval.json`).

use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{
    chunk_text, EmbeddingProvider, OnnxModelSpec, OnnxProvider, ONNX_ALL_MINILM,
    ONNX_ALL_MINILM_L12, ONNX_ALL_MINILM_L12_Q, ONNX_ALL_MINILM_Q, ONNX_ARCTIC_S_Q,
    ONNX_ARCTIC_XS_Q, ONNX_BGE_SMALL_EN_Q,
};
use serde::Deserialize;

/// Chunk budget every model is measured at, regardless of its own context
/// window — the production default (`EmbeddingsConfig::max_tokens`).
const FIXED_BUDGET_TOKENS: usize = 256;

/// Candidate models, with the HuggingFace repo id **and file** `fastembed`
/// pulls them from (carried here rather than queried, because this crate has no
/// `fastembed` dependency — only `engram-models` does). The file matters: a repo
/// hosting both an fp32 and a quantized export would otherwise have its size
/// reported as whichever is larger.
const CANDIDATES: &[(&str, OnnxModelSpec, &str, &str)] = &[
    (
        "minilm-q",
        ONNX_ALL_MINILM_Q,
        "Xenova/all-MiniLM-L6-v2",
        "model_quantized.onnx",
    ),
    (
        "minilm-fp32",
        ONNX_ALL_MINILM,
        "Qdrant/all-MiniLM-L6-v2-onnx",
        "model.onnx",
    ),
    (
        "arctic-xs-q",
        ONNX_ARCTIC_XS_Q,
        "snowflake/snowflake-arctic-embed-xs",
        "model_quantized.onnx",
    ),
    (
        "arctic-s-q",
        ONNX_ARCTIC_S_Q,
        "snowflake/snowflake-arctic-embed-s",
        "model_quantized.onnx",
    ),
    (
        "minilm-l12-q",
        ONNX_ALL_MINILM_L12_Q,
        "Xenova/all-MiniLM-L12-v2",
        "model_quantized.onnx",
    ),
    (
        "minilm-l12-fp32",
        ONNX_ALL_MINILM_L12,
        "Xenova/all-MiniLM-L12-v2",
        "model.onnx",
    ),
    (
        "bge-small-q",
        ONNX_BGE_SMALL_EN_Q,
        "Qdrant/bge-small-en-v1.5-onnx-Q",
        "model_optimized.onnx",
    ),
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
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

/// Bytes of the cached `file` under `repo`'s hub-cache directory — the weights
/// the daemon holds resident and a first run downloads. Matching the exact file
/// (not just the repo) matters: `Xenova/all-MiniLM-L12-v2` hosts both the fp32
/// and quantized exports, and reporting the larger for both made the int8
/// default look like a 127 MB download. Returns `None` when nothing matches (no
/// number beats a wrong number).
fn cached_weight_bytes(repo: &str, file: &str) -> Option<u64> {
    let root = engramdb::storage::paths::model_cache_dir().ok()?;
    let needle = format!("models--{}", repo.replace('/', "--"));
    let mut best = 0u64;
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
            } else if path.file_name().is_some_and(|n| n == file)
                && path.to_string_lossy().contains(&needle)
                && meta.len() > best
            {
                best = meta.len();
            }
        }
    }
    (best > 0).then_some(best)
}

struct Row {
    key: &'static str,
    model_id: String,
    disk_mb: Option<f64>,
    cold_ms: f64,
    warm_mean: f64,
    warm_p50: f64,
    warm_p95: f64,
    batch16_ms: f64,
    batch16_per_text: f64,
}

async fn bench_model(
    key: &'static str,
    spec: OnnxModelSpec,
    repo: &str,
    file: &str,
    docs: &[String],
    queries: &[String],
    iters: usize,
) -> Result<Row> {
    // Cold: session init + first embed, exactly what a daemon-less CLI pays.
    let t0 = Instant::now();
    let provider = OnnxProvider::with_model(spec)
        .with_context(|| format!("loading {key} (is it staged in the model cache?)"))?;
    provider.embed(&queries[0]).await?;
    let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Warm-up so the steady-state numbers exclude first-call allocation.
    for q in queries.iter().take(5) {
        provider.embed(q).await?;
    }

    let mut samples = Vec::with_capacity(iters);
    for i in 0..iters {
        let q = &queries[i % queries.len()];
        let t = Instant::now();
        provider.embed(q).await?;
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let warm_mean = samples.iter().sum::<f64>() / samples.len() as f64;
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Batch path: fixed batch of 16 identical-length-distribution chunks.
    let batches = (iters / 4).max(4);
    let mut batch_samples = Vec::with_capacity(batches);
    for i in 0..batches {
        let start = (i * 16) % docs.len().saturating_sub(16).max(1);
        let slice: Vec<&str> = docs[start..(start + 16).min(docs.len())]
            .iter()
            .map(|s| s.as_str())
            .collect();
        let t = Instant::now();
        provider.embed_batch(&slice).await?;
        batch_samples.push((t.elapsed().as_secs_f64() * 1000.0, slice.len()));
    }
    let batch16_ms =
        batch_samples.iter().map(|(ms, _)| ms).sum::<f64>() / batch_samples.len() as f64;
    let texts: usize = batch_samples.iter().map(|(_, n)| n).sum();
    let batch16_per_text =
        batch_samples.iter().map(|(ms, _)| ms).sum::<f64>() / texts.max(1) as f64;

    Ok(Row {
        key,
        model_id: provider.model_id(),
        disk_mb: cached_weight_bytes(repo, file).map(|b| b as f64 / 1_048_576.0),
        cold_ms,
        warm_mean,
        warm_p50: percentile(&samples, 0.50),
        warm_p95: percentile(&samples, 0.95),
        batch16_ms,
        batch16_per_text,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_path =
        std::env::var("EMBED_EVAL_DATA").unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let iters: usize = std::env::var("EMBED_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let filter: Option<Vec<String>> = std::env::var("EMBED_BENCH_MODELS")
        .ok()
        .map(|s| s.split(',').map(|m| m.trim().to_string()).collect());

    let raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("reading dataset {data_path}"))?;
    let ds: Dataset = serde_json::from_str(&raw).context("parsing dataset")?;

    // Identical inputs for every model: production composition, production
    // chunk budget. Sorted for a deterministic batch composition (int8 output
    // is mildly batch-sensitive).
    let mut docs: Vec<String> = ds
        .memories
        .iter()
        .flat_map(|m| chunk_text(&format!("{} {}", m.summary, m.content), FIXED_BUDGET_TOKENS))
        .collect();
    docs.sort();
    let queries: Vec<String> = ds.queries.iter().map(|q| q.text.clone()).collect();
    println!(
        "corpus: {} chunks @ {FIXED_BUDGET_TOKENS}-token budget, {} queries, {iters} warm iters",
        docs.len(),
        queries.len()
    );

    let mut rows = Vec::new();
    for (key, spec, repo, file) in CANDIDATES {
        if filter.as_ref().is_some_and(|f| !f.iter().any(|k| k == key)) {
            continue;
        }
        println!("--- {key} ---");
        match bench_model(key, spec.clone(), repo, file, &docs, &queries, iters).await {
            Ok(row) => rows.push(row),
            Err(e) => println!("  skipped: {e:#}"),
        }
    }

    println!(
        "\n{:<14} {:<30} {:>8} {:>9} {:>9} {:>9} {:>9} {:>11} {:>11}",
        "model",
        "model_id",
        "disk_MB",
        "cold_ms",
        "warm_mean",
        "warm_p50",
        "warm_p95",
        "batch16_ms",
        "ms/text_b16"
    );
    let baseline = rows
        .iter()
        .find(|r| r.key == "minilm-q")
        .map(|r| r.warm_mean);
    for r in &rows {
        println!(
            "{:<14} {:<30} {:>8} {:>9.1} {:>9.2} {:>9.2} {:>9.2} {:>11.1} {:>11.2}",
            r.key,
            r.model_id,
            r.disk_mb
                .map(|m| format!("{m:.0}"))
                .unwrap_or_else(|| "?".into()),
            r.cold_ms,
            r.warm_mean,
            r.warm_p50,
            r.warm_p95,
            r.batch16_ms,
            r.batch16_per_text,
        );
    }
    if let Some(base) = baseline {
        println!("\nwarm-mean relative to minilm-q (lower is faster):");
        for r in &rows {
            println!("  {:<14} {:.2}x", r.key, r.warm_mean / base);
        }
    }
    Ok(())
}
