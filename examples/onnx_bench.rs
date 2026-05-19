//! Comprehensive CPU vs Core ML comparison for EngramDB's ONNX workloads.
//!
//! The criterion `onnx_backend` group only measures isolated, warm,
//! single-call latency. This harness adds the dimensions that actually
//! decide whether Core ML is worth enabling for a long-lived daemon:
//!
//! 1. **Cold vs warm** — first call (model load + Core ML graph compile)
//!    vs steady state, quantifying how much a persistent daemon amortizes.
//! 2. **Steady-state percentiles** — mean / p50 / p95 / p99 over many warm
//!    iterations in one process (the daemon scenario).
//! 3. **Sustained throughput** — ops/sec over a fixed window.
//! 4. **Under CPU contention** — the same steady-state measurement while
//!    every core is saturated by background work, to see whether ANE/GPU
//!    offload wins when the CPU is busy (the realistic hook/daemon case).
//! 5. **Memory** — process RSS growth attributable to loading the model on
//!    each backend.
//!
//! Workloads: embedding single + batch16 (all-MiniLM-L6-v2), NLI
//! contradiction (DeBERTa-v3-xsmall), and T5 title generation
//! (Xenova/t5-small int8). Models download on first use; an unavailable
//! model is skipped, not fatal. Core ML only differs from CPU when built
//! `--features coreml` on macOS.
//!
//! Run with: `cargo run --release --features coreml --example onnx_bench`

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};
use engramdb::nli::{NliProvider, OnnxNliProvider};
use engramdb::onnx_ep::Backend;
use engramdb::title::t5::T5TitleGenerator;
use engramdb::title::TitleGenerator;
use engramdb::types::EngramConfig;

/// How long the sustained-throughput phase runs per workload.
const SUSTAINED: Duration = Duration::from_secs(5);

/// Representative inputs of varied length (short note -> long paragraph).
const INPUTS: &[&str] = &[
    "Use cargo nextest run instead of cargo test.",
    "Fixed a panic in the MCP server where concurrent tool calls poisoned the \
     embedding provider mutex; the provider is now cached behind an Arc.",
    "The retrieval engine scores each candidate by combining semantic similarity, \
     physical and logical scope proximity, a recency decay factor, and a provenance \
     trust weight, then optionally reranks the top fifty with a cross-encoder before \
     applying contradiction filtering against existing memories in the same scope.",
];

/// Resident set size of this process in KiB (via `ps`, no extra deps).
fn rss_kib() -> u64 {
    let pid = std::process::id();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

struct Spinners {
    stop: Arc<AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl Spinners {
    /// Saturate every available core with busy work until dropped.
    fn saturate() -> Self {
        let cores = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let stop = Arc::new(AtomicBool::new(false));
        let handles = (0..cores)
            .map(|_| {
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut acc = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        acc = std::hint::black_box(
                            acc.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                })
            })
            .collect();
        Self { stop, handles }
    }
}

impl Drop for Spinners {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

#[derive(Default)]
struct Percentiles {
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
}

fn summarize(mut samples: Vec<Duration>) -> Percentiles {
    if samples.is_empty() {
        return Percentiles::default();
    }
    samples.sort_unstable();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| ms(samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)]);
    let mean = samples.iter().sum::<Duration>().as_secs_f64() * 1000.0 / samples.len() as f64;
    Percentiles {
        mean_ms: mean,
        p50_ms: at(0.50),
        p95_ms: at(0.95),
        p99_ms: at(0.99),
    }
}

/// Run `iters` timed calls, cycling through the realistic input mix.
async fn timed<MakeFut, Fut>(iters: usize, infer: &MakeFut) -> Result<Vec<Duration>>
where
    MakeFut: Fn(&'static str) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut samples = Vec::with_capacity(iters);
    for i in 0..iters {
        let input = INPUTS[i % INPUTS.len()];
        let start = Instant::now();
        infer(input).await?;
        samples.push(start.elapsed());
    }
    Ok(samples)
}

/// Full phase suite for one (workload, backend) pair.
async fn bench<MakeFut, Fut>(
    workload: &str,
    backend_label: &str,
    rss_before: u64,
    warm_iters: usize,
    contended_iters: usize,
    infer: MakeFut,
) -> Result<()>
where
    MakeFut: Fn(&'static str) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    // Cold first call: the model is loaded; this pays first-inference cost
    // (incl. Core ML graph compilation), which a daemon pays only once.
    let cold_start = Instant::now();
    infer(INPUTS[0]).await?;
    let cold_ms = cold_start.elapsed().as_secs_f64() * 1000.0;
    let rss_loaded = rss_kib();

    // Warm steady state.
    let _ = timed(warm_iters / 4, &infer).await?; // extra warm-up
    let warm = summarize(timed(warm_iters, &infer).await?);

    // Sustained throughput over a fixed window.
    let sustained_start = Instant::now();
    let mut ops = 0u64;
    while sustained_start.elapsed() < SUSTAINED {
        infer(INPUTS[ops as usize % INPUTS.len()]).await?;
        ops += 1;
    }
    let ops_per_sec = ops as f64 / sustained_start.elapsed().as_secs_f64();

    // Steady state while every core is saturated by background work.
    let contended = {
        let _spinners = Spinners::saturate();
        // let the scheduler settle under load before measuring
        let _ = timed(contended_iters / 2, &infer).await?;
        summarize(timed(contended_iters, &infer).await?)
    };

    println!(
        "  {workload:<14} {backend_label:<7} | load+1st {cold_ms:8.1}ms | \
         warm mean {:7.2} p50 {:7.2} p95 {:7.2} p99 {:7.2} ms | \
         {ops_per_sec:7.1} ops/s | contended mean {:7.2} p99 {:7.2} ms | \
         RSS +{:.0} MiB",
        warm.mean_ms,
        warm.p50_ms,
        warm.p95_ms,
        warm.p99_ms,
        contended.mean_ms,
        contended.p99_ms,
        (rss_loaded.saturating_sub(rss_before)) as f64 / 1024.0,
    );
    Ok(())
}

/// Await one workload's phase suite, recording a FAILED row instead of
/// aborting the run — e.g. T5 on Core ML, whose dynamic-shape decoder the
/// Core ML EP cannot execute ("Unable to compute the prediction").
async fn report(workload: &str, backend_label: &str, fut: impl Future<Output = Result<()>>) {
    if let Err(error) = fut.await {
        println!("  {workload:<14} {backend_label:<7} | FAILED: {error}");
    }
}

async fn run_backend(backend_label: &str, backend: Backend) {
    println!("\n=== backend: {backend_label} ===");
    let nli_repo = EngramConfig::default().nli.model;

    let base_rss = rss_kib();
    if let Some(provider) = OnnxProvider::try_new_on(backend) {
        report(
            "embed_single",
            backend_label,
            bench("embed_single", backend_label, base_rss, 300, 60, |t| {
                let p = &provider;
                async move {
                    p.embed(t).await?;
                    Ok(())
                }
            }),
        )
        .await;

        let batch: Vec<&str> = INPUTS
            .iter()
            .flat_map(|s| std::iter::repeat_n(*s, 6))
            .collect();
        report(
            "embed_batch",
            backend_label,
            bench("embed_batch", backend_label, base_rss, 120, 30, |_| {
                let p = &provider;
                let batch = &batch;
                async move {
                    p.embed_batch(batch).await?;
                    Ok(())
                }
            }),
        )
        .await;
    } else {
        println!("  embedding model unavailable, skipping");
    }

    let base_rss = rss_kib();
    if let Some(provider) = OnnxNliProvider::try_new_on(&nli_repo, backend) {
        report(
            "nli_classify",
            backend_label,
            bench("nli_classify", backend_label, base_rss, 80, 25, |t| {
                let p = &provider;
                async move {
                    p.classify("The database uses PostgreSQL.", t).await?;
                    Ok(())
                }
            }),
        )
        .await;
    } else {
        println!("  NLI model unavailable, skipping");
    }

    let base_rss = rss_kib();
    if let Some(generator) = T5TitleGenerator::try_new_on(backend) {
        report(
            "t5_title",
            backend_label,
            bench("t5_title", backend_label, base_rss, 24, 10, |t| {
                let g = &generator;
                async move {
                    g.generate(t).await?;
                    Ok(())
                }
            }),
        )
        .await;
    } else {
        println!("  T5 model unavailable, skipping");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("EngramDB ONNX backend benchmark");
    println!(
        "Core ML compiled in: {} | XNNPACK compiled in: {}",
        engramdb::onnx_ep::coreml_available(),
        engramdb::onnx_ep::xnnpack_available()
    );

    run_backend("cpu", Backend::Cpu).await;
    if engramdb::onnx_ep::coreml_available() {
        run_backend("coreml", Backend::CoreMl).await;
    }
    if engramdb::onnx_ep::xnnpack_available() {
        run_backend("xnnpack", Backend::Xnnpack).await;
    }

    println!("\nDone. Compare warm vs contended, sustained ops/s, and RSS across backends.");
    Ok(())
}
