//! Phase 1 of the ONNX Runtime -> pure-Rust evaluation: all-MiniLM-L12-v2
//! sentence embeddings on ONNX Runtime (uint8 + fp32), Burn (Flex, NdArray)
//! and Candle. See README.md for the method and how to run it.
//!
//! `bench <engine>` runs ONE engine in this process (so memory readings are
//! not polluted by another engine) and writes `results/<engine>.json`.
//! `compare` reads every result file and writes `results/summary.md`.

mod common;
#[cfg(any(feature = "burn-flex", feature = "burn-ndarray"))]
mod engine_burn;
#[cfg(feature = "candle")]
mod engine_candle;
#[cfg(feature = "ort")]
mod engine_ort;

use anyhow::{bail, Context, Result};
use common::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize, Deserialize)]
struct Report {
    engine: String,
    weights: String,
    weights_bytes: u64,
    threads: usize,
    load_ms: f64,
    first_call_ms: f64,
    rss_start_mib: f64,
    rss_after_load_mib: f64,
    peak_rss_after_load_mib: f64,
    peak_rss_end_mib: f64,
    query_single: Latency,
    doc_single: Latency,
    batch16: Latency,
    corpus_docs_per_sec: f64,
    mean_doc_tokens: f64,
    determinism: Determinism,
    query_ids: Vec<String>,
    query_vecs: Vec<Vec<f32>>,
    doc_ids: Vec<String>,
    doc_vecs: Vec<Vec<f32>>,
}

#[derive(Serialize, Deserialize)]
struct Determinism {
    trials: usize,
    load_threads: usize,
    distinct_vectors: usize,
    min_cosine_to_first: f64,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str, default: &str| -> PathBuf {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default))
    };
    let models = flag("--models", "models");
    let data = flag("--data", "../../examples/data/embed_eval.json");
    let out = flag("--out", "results");
    match args.first().map(String::as_str) {
        Some("bench") => {
            let engine = args.get(1).context("bench <engine>")?;
            bench(engine, &models, &data, &out)
        }
        Some("compare") => compare(&data, &out),
        _ => bail!(
            "usage: inference-engines bench <ort-u8|ort-f32|burn-flex|burn-ndarray|candle> \
             [--models DIR] [--data FILE] [--out DIR]\n       inference-engines compare [--out DIR]"
        ),
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Builds the requested engine. Returns the engine, the weights file it
/// read, and the thread count it uses.
fn load_engine(name: &str, models: &Path) -> Result<(Box<dyn Engine>, PathBuf)> {
    let _ = models;
    match name {
        #[cfg(feature = "ort")]
        "ort-u8" | "ort-f32" => {
            let file = if name == "ort-u8" {
                "model_uint8.onnx"
            } else {
                "model.onnx"
            };
            let path = models.join("xenova/onnx").join(file);
            let threads = std::env::var("ENGINE_THREADS").ok().and_then(|v| v.parse().ok());
            Ok((Box::new(engine_ort::OrtEngine::load(&path, threads)?), path))
        }
        #[cfg(feature = "burn-flex")]
        "burn-flex" => {
            let dir = models.join("st");
            Ok((
                Box::new(engine_burn::BurnEngine::load(&dir)?),
                dir.join("model.safetensors"),
            ))
        }
        #[cfg(all(feature = "burn-ndarray", not(feature = "burn-flex")))]
        "burn-ndarray" => {
            let dir = models.join("st");
            Ok((
                Box::new(engine_burn::BurnEngine::load(&dir)?),
                dir.join("model.safetensors"),
            ))
        }
        #[cfg(feature = "candle")]
        "candle" => {
            let dir = models.join("st");
            Ok((
                Box::new(engine_candle::CandleEngine::load(&dir)?),
                dir.join("model.safetensors"),
            ))
        }
        other => bail!("engine `{other}` is not compiled into this binary (check --features)"),
    }
}

fn bench(name: &str, models: &Path, data: &Path, out: &Path) -> Result<()> {
    let iters = env_usize("ITERS", 3);
    let ds = load_dataset(data)?;
    let tok = load_tokenizer(&models.join("xenova/tokenizer.json"))?;
    let docs: Vec<String> = ds.memories.iter().map(|m| m.doc()).collect();
    let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let queries: Vec<&str> = ds.queries.iter().map(|q| q.text.as_str()).collect();

    // --- Load ------------------------------------------------------------
    let (rss_start, _) = rss_mib();
    let t = Instant::now();
    let (mut engine, weights) = load_engine(name, models)?;
    let load_ms = ms(t);
    let (rss_after_load, peak_after_load) = rss_mib();
    let weights_bytes = std::fs::metadata(&weights).map(|m| m.len()).unwrap_or(0);
    eprintln!("[{name}] loaded in {load_ms:.1} ms");

    // --- First call (cold kernels, lazy allocations) -----------------------
    let t = Instant::now();
    embed(engine.as_mut(), &tok, &[queries[0]])?;
    let first_call_ms = ms(t);

    // Warm-up.
    for q in queries.iter().take(5) {
        embed(engine.as_mut(), &tok, &[q])?;
    }

    // --- Single query (short text; the query path) ------------------------
    let mut samples = Vec::new();
    for _ in 0..iters {
        for q in &queries {
            let t = Instant::now();
            embed(engine.as_mut(), &tok, &[q])?;
            samples.push(ms(t));
        }
    }
    let query_single = Latency::from_samples(samples);
    eprintln!("[{name}] query single p50 {:.2} ms", query_single.p50_ms);

    // --- Single document (long text; the create path) ---------------------
    let mut samples = Vec::new();
    for _ in 0..iters {
        for d in &doc_refs {
            let t = Instant::now();
            embed(engine.as_mut(), &tok, &[d])?;
            samples.push(ms(t));
        }
    }
    let doc_single = Latency::from_samples(samples);
    eprintln!("[{name}] doc single p50 {:.2} ms", doc_single.p50_ms);

    // --- Batch of 16 documents (reindex path) -----------------------------
    let mut samples = Vec::new();
    for _ in 0..iters * 4 {
        let t = Instant::now();
        embed(engine.as_mut(), &tok, &doc_refs[..16])?;
        samples.push(ms(t));
    }
    let batch16 = Latency::from_samples(samples);
    eprintln!("[{name}] batch16 p50 {:.2} ms", batch16.p50_ms);

    // --- Whole corpus in batches of 16: throughput + the vectors we keep ---
    let t = Instant::now();
    let mut doc_vecs = Vec::new();
    for chunk in doc_refs.chunks(16) {
        doc_vecs.extend(embed(engine.as_mut(), &tok, chunk)?);
    }
    let corpus_docs_per_sec = doc_refs.len() as f64 / t.elapsed().as_secs_f64();
    let mut query_vecs = Vec::new();
    for q in &queries {
        query_vecs.extend(embed(engine.as_mut(), &tok, &[q])?);
    }
    let mean_doc_tokens = doc_refs
        .iter()
        .map(|d| encode(&tok, &[d]).map(|b| b.seq as f64).unwrap_or(0.0))
        .sum::<f64>()
        / doc_refs.len() as f64;

    // --- Determinism under CPU contention ---------------------------------
    // EngramDB found a runtime build that returned different vectors for the
    // same text under load (CLAUDE.md, R6/R9), so this is a pass/fail check.
    let load_threads = env_usize("PROBE_LOAD_THREADS", 4);
    let trials = env_usize("PROBE_TRIALS", 30);
    let stop = Arc::new(AtomicBool::new(false));
    let spinners: Vec<_> = (0..load_threads)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                    std::hint::black_box(x);
                }
            })
        })
        .collect();
    let mut probe = Vec::new();
    for _ in 0..trials {
        probe.extend(embed(engine.as_mut(), &tok, &[doc_refs[0]])?);
    }
    stop.store(true, Ordering::Relaxed);
    for s in spinners {
        let _ = s.join();
    }
    let distinct: HashSet<Vec<u32>> = probe
        .iter()
        .map(|v| v.iter().map(|x| x.to_bits()).collect())
        .collect();
    let determinism = Determinism {
        trials,
        load_threads,
        distinct_vectors: distinct.len(),
        min_cosine_to_first: probe
            .iter()
            .map(|v| cosine(v, &probe[0]))
            .fold(f64::INFINITY, f64::min),
    };

    let (_, peak_end) = rss_mib();
    let report = Report {
        engine: name.to_string(),
        weights: weights.display().to_string(),
        weights_bytes,
        threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        load_ms,
        first_call_ms,
        rss_start_mib: rss_start,
        rss_after_load_mib: rss_after_load,
        peak_rss_after_load_mib: peak_after_load,
        peak_rss_end_mib: peak_end,
        query_single,
        doc_single,
        batch16,
        corpus_docs_per_sec,
        mean_doc_tokens,
        determinism,
        query_ids: ds.queries.iter().map(|q| q.id.clone()).collect(),
        query_vecs,
        doc_ids: ds.memories.iter().map(|m| m.id.clone()).collect(),
        doc_vecs,
    };
    std::fs::create_dir_all(out)?;
    let path = out.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec(&report)?)?;
    eprintln!("[{name}] wrote {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Quality {
    p1: f64,
    r5: f64,
    mrr10: f64,
    ndcg10: f64,
}

/// Same definitions as `examples/embed_matrix.rs`, so numbers line up with
/// the repo's earlier model evaluations.
fn retrieval_quality(r: &Report, ds: &Dataset) -> Quality {
    let mut q = Quality::default();
    let n = ds.queries.len() as f64;
    for (qi, query) in ds.queries.iter().enumerate() {
        let mut scored: Vec<(f64, &str)> = r
            .doc_vecs
            .iter()
            .zip(&r.doc_ids)
            .map(|(d, id)| (cosine(&r.query_vecs[qi], d), id.as_str()))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let ranked: Vec<&str> = scored.iter().map(|s| s.1).collect();
        let rel = |id: &str| *query.relevant.get(id).unwrap_or(&0);
        q.p1 += f64::from(ranked.first().is_some_and(|id| rel(id) >= 1));
        let total = query.relevant.values().filter(|g| **g >= 1).count();
        let hits = ranked.iter().take(5).filter(|id| rel(id) >= 1).count();
        if total > 0 {
            q.r5 += hits as f64 / total.min(5) as f64;
        }
        if let Some(i) = ranked.iter().take(10).position(|id| rel(id) == 2) {
            q.mrr10 += 1.0 / (i + 1) as f64;
        }
        let dcg: f64 = ranked
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, id)| rel(id) as f64 / ((i + 2) as f64).log2())
            .sum();
        let mut ideal: Vec<f64> = query.relevant.values().map(|g| *g as f64).collect();
        ideal.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let idcg: f64 = ideal
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, g)| g / ((i + 2) as f64).log2())
            .sum();
        if idcg > 0.0 {
            q.ndcg10 += dcg / idcg;
        }
    }
    Quality {
        p1: q.p1 / n,
        r5: q.r5 / n,
        mrr10: q.mrr10 / n,
        ndcg10: q.ndcg10 / n,
    }
}

/// `build.tsv`, written by `run.sh`: engine, build seconds, binary bytes.
fn read_build_info(out: &Path) -> BTreeMap<String, (f64, u64)> {
    let mut m = BTreeMap::new();
    if let Ok(s) = std::fs::read_to_string(out.join("build.tsv")) {
        for line in s.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if let [e, secs, bytes] = f[..] {
                if let (Ok(s), Ok(b)) = (secs.parse(), bytes.parse()) {
                    m.insert(e.to_string(), (s, b));
                }
            }
        }
    }
    m
}

const ORDER: [&str; 5] = ["ort-u8", "ort-f32", "burn-flex", "burn-ndarray", "candle"];

fn compare(data: &Path, out: &Path) -> Result<()> {
    let ds = load_dataset(data)?;
    let mut reports: Vec<Report> = Vec::new();
    for name in ORDER {
        let p = out.join(format!("{name}.json"));
        if let Ok(bytes) = std::fs::read(&p) {
            reports.push(serde_json::from_slice(&bytes).with_context(|| p.display().to_string())?);
        }
    }
    if reports.is_empty() {
        bail!("no result files in {}", out.display());
    }
    // Reference for numerical agreement: ONNX Runtime fp32 (same weights and
    // precision as the Burn/Candle runs, so any gap is the engine).
    let reference = reports
        .iter()
        .position(|r| r.engine == "ort-f32")
        .unwrap_or(0);
    let builds = read_build_info(out);
    let mib = |b: u64| b as f64 / (1024.0 * 1024.0);

    let mut md = String::new();
    let push = |md: &mut String, s: String| {
        md.push_str(&s);
        md.push('\n');
    };
    push(&mut md, "## Speed\n".into());
    push(&mut md, "| Engine | Load (ms) | First call (ms) | Query p50 / p95 (ms) | Doc p50 / p95 (ms) | Batch-16 p50 (ms) | Corpus (docs/s) |".into());
    push(&mut md, "|---|---:|---:|---:|---:|---:|---:|".into());
    for r in &reports {
        push(&mut md, format!(
            "| {} | {:.0} | {:.1} | {:.2} / {:.2} | {:.1} / {:.1} | {:.1} | {:.1} |",
            r.engine, r.load_ms, r.first_call_ms, r.query_single.p50_ms, r.query_single.p95_ms,
            r.doc_single.p50_ms, r.doc_single.p95_ms, r.batch16.p50_ms, r.corpus_docs_per_sec
        ));
    }
    push(&mut md, "\n## Memory and size\n".into());
    push(&mut md, "| Engine | RSS after load (MiB) | Peak RSS (MiB) | Weights file (MiB) | Binary (MiB) | Clean release build (s) |".into());
    push(&mut md, "|---|---:|---:|---:|---:|---:|".into());
    for r in &reports {
        let family = match r.engine.as_str() {
            "ort-u8" | "ort-f32" => "ort",
            e => e,
        };
        let (secs, bytes) = builds
            .get(family)
            .map(|(s, b)| (format!("{s:.0}"), format!("{:.1}", mib(*b))))
            .unwrap_or(("–".into(), "–".into()));
        push(&mut md, format!(
            "| {} | {:.0} | {:.0} | {:.1} | {} | {} |",
            r.engine, r.rss_after_load_mib, r.peak_rss_end_mib, mib(r.weights_bytes), bytes, secs
        ));
    }
    if let Some((s, b)) = builds.get("baseline") {
        push(&mut md, format!(
            "\nBaseline binary with no engine (tokenizer + harness only): {:.1} MiB, {s:.0} s.",
            mib(*b)
        ));
    }
    push(&mut md, format!(
        "\n## Agreement and quality (reference: {})\n",
        reports[reference].engine
    ));
    push(&mut md, "| Engine | Mean cosine | Min cosine | P@1 | R@5 | MRR@10 | nDCG@10 | Determinism (distinct / trials) |".into());
    push(&mut md, "|---|---:|---:|---:|---:|---:|---:|---:|".into());
    let rf = &reports[reference];
    for r in &reports {
        let cos: Vec<f64> = r
            .doc_vecs
            .iter()
            .zip(&rf.doc_vecs)
            .chain(r.query_vecs.iter().zip(&rf.query_vecs))
            .map(|(a, b)| cosine(a, b))
            .collect();
        let mean = cos.iter().sum::<f64>() / cos.len() as f64;
        let min = cos.iter().cloned().fold(f64::INFINITY, f64::min);
        let q = retrieval_quality(r, &ds);
        push(&mut md, format!(
            "| {} | {:.6} | {:.6} | {:.3} | {:.3} | {:.3} | {:.3} | {} / {} |",
            r.engine, mean, min, q.p1, q.r5, q.mrr10, q.ndcg10,
            r.determinism.distinct_vectors, r.determinism.trials
        ));
    }
    let r0 = &reports[0];
    push(&mut md, format!(
        "\n{} threads available; mean document length {:.0} tokens (max {}).",
        r0.threads, r0.mean_doc_tokens, MAX_TOKENS
    ));
    print!("{md}");
    std::fs::write(out.join("summary.md"), &md)?;
    Ok(())
}
