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
//! 6. **Lever E — concurrent submission** — K tokio tasks each submitting
//!    embed requests simultaneously to a shared single-session provider
//!    (the current architecture) vs a naive pool of N independent sessions.
//!    Measures aggregate throughput (ops/s) and per-request latency at
//!    K=1,2,4,8 concurrency for both single-session and pool-of-2.
//!    Run with `ENGRAMDB_BENCH_WORKLOADS=lever_e`.
//!
//! Workloads: embedding single + batch16 (all-MiniLM-L6-v2), NLI
//! contradiction (DeBERTa-v3-xsmall), and T5 title generation
//! (Xenova/t5-small int8). Models download on first use; an unavailable
//! model is skipped, not fatal. Core ML only differs from CPU when built
//! `--features coreml` on macOS.
//!
//! Run with: `cargo run --release --features coreml --example onnx_bench`
//! Run Lever E only: `ENGRAMDB_BENCH_WORKLOADS=lever_e cargo run --release --example onnx_bench`

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use engramdb::embeddings::{EmbeddingProvider, OnnxProvider, ONNX_ALL_MINILM, ONNX_ALL_MINILM_Q};
use engramdb::nli::{NliProvider, OnnxNliProvider, NLI_DEBERTA_XSMALL, NLI_DEBERTA_XSMALL_Q};
use engramdb::onnx_ep::Backend;
use engramdb::title::t5::T5TitleGenerator;
use engramdb::title::TitleGenerator;

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

/// Workload filter via `ENGRAMDB_BENCH_WORKLOADS` (comma list); all if unset.
fn enabled(workload: &str) -> bool {
    match std::env::var("ENGRAMDB_BENCH_WORKLOADS") {
        Ok(list) => list.split(',').any(|w| w.trim() == workload),
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------------
// Lever E: concurrent-submission scenario
// ---------------------------------------------------------------------------

/// Run K tokio tasks concurrently for `window` seconds, each repeatedly
/// calling `embed` on a shared provider. Returns (total_ops, latency_samples).
async fn concurrent_window(
    provider: Arc<OnnxProvider>,
    concurrency: usize,
    window: Duration,
) -> (u64, Vec<Duration>) {
    let total_ops = Arc::new(AtomicU64::new(0));
    let all_samples: Arc<tokio::sync::Mutex<Vec<Duration>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let deadline = Instant::now() + window;

    let mut handles = Vec::with_capacity(concurrency);
    for task_idx in 0..concurrency {
        let p = Arc::clone(&provider);
        let ops_counter = Arc::clone(&total_ops);
        let samples_out = Arc::clone(&all_samples);
        handles.push(tokio::spawn(async move {
            let mut local_samples = Vec::new();
            let mut i = 0usize;
            while Instant::now() < deadline {
                let input = INPUTS[(task_idx + i) % INPUTS.len()];
                let t = Instant::now();
                // Ignore errors — the session mutex may be contended; we count
                // only successful calls in throughput.
                if p.embed(input).await.is_ok() {
                    local_samples.push(t.elapsed());
                    ops_counter.fetch_add(1, Ordering::Relaxed);
                }
                i += 1;
            }
            let mut guard = samples_out.lock().await;
            guard.extend(local_samples);
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    let ops = total_ops.load(Ordering::Relaxed);
    let samples = Arc::try_unwrap(all_samples)
        .unwrap_or_else(|a| {
            // Arc still shared — shouldn't happen after all tasks joined
            tokio::sync::Mutex::new(a.blocking_lock().clone())
        })
        .into_inner();
    (ops, samples)
}

/// Lever E: single-session (current arch) vs naive pool-of-N, at K=1,2,4,8.
///
/// The single-session provider serialises all concurrent callers through its
/// `Arc<Mutex<TextEmbedding>>`. A pool lets up to N callers run in parallel,
/// but only if `N * intra_threads <= physical_cores` (else oversubscription).
///
/// Prints a table: K | single-session ops/s mean-ms p99-ms | pool-2 ops/s mean-ms p99-ms
async fn bench_lever_e() {
    let intra = engramdb::onnx_ep::intra_threads();
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    println!(
        "\n=== Lever E: concurrent embedding (single-session vs pool-of-2) ===\
         \n    machine: {cores} cores | intra_threads (NLI/T5)={intra} | fastembed manages its own pool\
         \n    NOTE: fastembed OnnxProvider uses Arc<Mutex<TextEmbedding>> — concurrency\
         \n          serialises through that mutex; pool removes the bottleneck at RAM cost."
    );

    let single_session = match OnnxProvider::with_model(engramdb::embeddings::ONNX_ALL_MINILM_Q) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            println!("  embedding model unavailable, skipping Lever E: {e}");
            return;
        }
    };

    // Pool-of-2: two independent OnnxProvider sessions, requests round-robin.
    // Each session has its own TextEmbedding (ONNX session + weights).
    let pool: Vec<Arc<OnnxProvider>> = (0..2)
        .filter_map(|_| {
            OnnxProvider::with_model(engramdb::embeddings::ONNX_ALL_MINILM_Q)
                .ok()
                .map(Arc::new)
        })
        .collect();
    if pool.len() < 2 {
        println!("  could not create 2nd session for pool, skipping pool column");
    }

    // Warm up both to ensure ONNX session is hot.
    let _ = single_session.embed(INPUTS[0]).await;
    for p in &pool {
        let _ = p.embed(INPUTS[0]).await;
    }

    let window = Duration::from_secs(8);
    println!(
        "\n  {:>3}  {:>14}  {:>9}  {:>9}  |  {:>14}  {:>9}  {:>9}",
        "K", "single ops/s", "mean ms", "p99 ms", "pool-2 ops/s", "mean ms", "p99 ms"
    );
    println!("  {}", "-".repeat(80));

    for &k in &[1usize, 2, 4, 8] {
        // Single session: all K tasks contend on the same Mutex<TextEmbedding>.
        let (s_ops, s_samples) = concurrent_window(Arc::clone(&single_session), k, window).await;
        let s_stats = summarize(s_samples);
        let s_throughput = s_ops as f64 / window.as_secs_f64();

        // Pool-of-2: tasks round-robin across sessions; avoids the single mutex.
        let (p_ops, p_samples) = if pool.len() >= 2 {
            // Wrap the round-robin in a simple AtomicU64 index.
            let idx = Arc::new(AtomicU64::new(0));
            let p_total_ops = Arc::new(AtomicU64::new(0));
            let p_all_samples: Arc<tokio::sync::Mutex<Vec<Duration>>> =
                Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let deadline = Instant::now() + window;
            let mut handles = Vec::with_capacity(k);
            for task_i in 0..k {
                let pool_ref: Vec<Arc<OnnxProvider>> = pool.clone();
                let counter = Arc::clone(&p_total_ops);
                let samples_out = Arc::clone(&p_all_samples);
                let idx_ref = Arc::clone(&idx);
                handles.push(tokio::spawn(async move {
                    let mut local = Vec::new();
                    let mut i = task_i;
                    while Instant::now() < deadline {
                        let slot =
                            idx_ref.fetch_add(1, Ordering::Relaxed) as usize % pool_ref.len();
                        let p = &pool_ref[slot];
                        let input = INPUTS[i % INPUTS.len()];
                        let t = Instant::now();
                        if p.embed(input).await.is_ok() {
                            local.push(t.elapsed());
                            counter.fetch_add(1, Ordering::Relaxed);
                        }
                        i += 1;
                    }
                    let mut g = samples_out.lock().await;
                    g.extend(local);
                }));
            }
            for h in handles {
                let _ = h.await;
            }
            let total = p_total_ops.load(Ordering::Relaxed);
            let samples = Arc::try_unwrap(p_all_samples)
                .map(|m| m.into_inner())
                .unwrap_or_default();
            (total, samples)
        } else {
            (0, Vec::new())
        };
        let p_stats = summarize(p_samples);
        let p_throughput = p_ops as f64 / window.as_secs_f64();

        println!(
            "  {:>3}  {:>14.1}  {:>9.2}  {:>9.2}  |  {:>14.1}  {:>9.2}  {:>9.2}",
            k,
            s_throughput,
            s_stats.mean_ms,
            s_stats.p99_ms,
            p_throughput,
            p_stats.mean_ms,
            p_stats.p99_ms,
        );
    }

    // Interpretation note.
    println!(
        "\n  Interpretation:\
         \n  - If single-session throughput plateaus (K=2..8 ~= K=1): mutex is the bottleneck;\
         \n    pool-of-2 should show ~2x improvement at K=2 and level off.\
         \n  - If single throughput scales with K: fastembed's internal ORT intra-op\
         \n    parallelism already saturates the cores; pool adds RAM for no gain.\
         \n  - Oversubscription point: {cores} cores / {intra} intra_threads = {} parallel ORT sessions\
         \n    before threads compete. Beyond this, pool degrades.",
        cores / intra.max(1)
    );
}

async fn run_backend(backend_label: &str, backend: Backend) {
    println!("\n=== backend: {backend_label} ===");

    // fp32 vs int8 all-MiniLM A/B (Lever B). "_q" = int8 quantized.
    for (suffix, spec) in [("", ONNX_ALL_MINILM), ("_q", ONNX_ALL_MINILM_Q)] {
        let single = format!("embed_single{suffix}");
        let batched = format!("embed_batch{suffix}");
        if !enabled(&single) && !enabled(&batched) {
            continue;
        }
        let base_rss = rss_kib();
        match OnnxProvider::with_model_on(spec, backend) {
            Ok(provider) => {
                if enabled(&single) {
                    report(
                        &single,
                        backend_label,
                        bench(&single, backend_label, base_rss, 300, 60, |t| {
                            let p = &provider;
                            async move {
                                p.embed(t).await?;
                                Ok(())
                            }
                        }),
                    )
                    .await;
                }
                if enabled(&batched) {
                    let batch: Vec<&str> = INPUTS
                        .iter()
                        .flat_map(|s| std::iter::repeat_n(*s, 6))
                        .collect();
                    report(
                        &batched,
                        backend_label,
                        bench(&batched, backend_label, base_rss, 120, 30, |_| {
                            let p = &provider;
                            let batch = &batch;
                            async move {
                                p.embed_batch(batch).await?;
                                Ok(())
                            }
                        }),
                    )
                    .await;
                }
            }
            Err(_) => println!("  embedding model '{single}' unavailable, skipping"),
        }
    }

    // fp32 vs int8 NLI cross-encoder A/B (Lever D). "_q" = int8 quantized.
    for (suffix, spec) in [("", NLI_DEBERTA_XSMALL), ("_q", NLI_DEBERTA_XSMALL_Q)] {
        let w = format!("nli_classify{suffix}");
        if !enabled(&w) {
            continue;
        }
        let base_rss = rss_kib();
        match OnnxNliProvider::with_spec_on(&spec, backend) {
            Ok(provider) => {
                report(
                    &w,
                    backend_label,
                    bench(&w, backend_label, base_rss, 80, 25, |t| {
                        let p = &provider;
                        async move {
                            p.classify("The database uses PostgreSQL.", t).await?;
                            Ok(())
                        }
                    }),
                )
                .await;
            }
            Err(_) => println!("  NLI model '{w}' unavailable, skipping"),
        }
    }

    if enabled("t5_title") {
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
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("EngramDB ONNX backend benchmark");
    println!(
        "Core ML: {} | XNNPACK: {} | NLI/T5 intra_threads: {}",
        engramdb::onnx_ep::coreml_available(),
        engramdb::onnx_ep::xnnpack_available(),
        engramdb::onnx_ep::intra_threads()
    );

    if enabled("lever_e") {
        bench_lever_e().await;
    }

    // Skip the per-backend suite when only lever_e was requested.
    let only_lever_e = std::env::var("ENGRAMDB_BENCH_WORKLOADS")
        .map(|v| v.split(',').all(|w| w.trim() == "lever_e"))
        .unwrap_or(false);
    if !only_lever_e {
        run_backend("cpu", Backend::Cpu).await;
        if engramdb::onnx_ep::coreml_available() {
            run_backend("coreml", Backend::CoreMl).await;
        }
        if engramdb::onnx_ep::xnnpack_available() {
            run_backend("xnnpack", Backend::Xnnpack).await;
        }
    }

    println!("\nDone. Compare warm vs contended, sustained ops/s, and RSS across backends.");
    Ok(())
}
