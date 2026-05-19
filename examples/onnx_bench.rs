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
//!    requests simultaneously to a shared single-session provider (the current
//!    architecture) vs a naive pool of N independent sessions. Covers all three
//!    model families: embedding (all-MiniLM-Q), NLI (DeBERTa-Q), and T5 title.
//!    Run with `ENGRAMDB_BENCH_WORKLOADS=lever_e`.
//! 7. **Part B — prepacked_weights A/B** — warm and concurrent latency with vs
//!    without PrepackedWeights for NLI and T5 (the two Mutex-guarded direct-ort
//!    sessions now default-on). Run with `ENGRAMDB_BENCH_WORKLOADS=prepacked_ab`.
//!
//! Workloads: embedding single + batch16 (all-MiniLM-L6-v2), NLI
//! contradiction (DeBERTa-v3-xsmall), and T5 title generation
//! (Xenova/t5-small int8). Models download on first use; an unavailable
//! model is skipped, not fatal. Core ML only differs from CPU when built
//! `--features coreml` on macOS.
//!
//! Run with: `cargo run --release --features coreml --example onnx_bench`
//! Run Lever E only: `ENGRAMDB_BENCH_WORKLOADS=lever_e cargo run --release --example onnx_bench`
//! Run prepacked A/B: `ENGRAMDB_BENCH_WORKLOADS=prepacked_ab cargo run --release --example onnx_bench`

use std::borrow::Cow;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// How long each Lever E / prepacked A/B concurrency window runs.
const CONCURRENT_WINDOW: Duration = Duration::from_secs(8);

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
// Lever E: concurrent-submission scenario — embeddings
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

// ---------------------------------------------------------------------------
// Lever E: NLI and T5 concurrent-submission scenario
// ---------------------------------------------------------------------------

/// Run K tokio tasks sharing one NLI provider for `window`. Returns (ops, samples).
async fn concurrent_nli_window(
    provider: Arc<OnnxNliProvider>,
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
            let mut local = Vec::new();
            let mut i = 0usize;
            while Instant::now() < deadline {
                let premise = INPUTS[(task_idx + i) % INPUTS.len()];
                let hypothesis = INPUTS[(task_idx + i + 1) % INPUTS.len()];
                let t = Instant::now();
                if p.classify(premise, hypothesis).await.is_ok() {
                    local.push(t.elapsed());
                    ops_counter.fetch_add(1, Ordering::Relaxed);
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
    let ops = total_ops.load(Ordering::Relaxed);
    let samples = Arc::try_unwrap(all_samples)
        .unwrap_or_else(|a| tokio::sync::Mutex::new(a.blocking_lock().clone()))
        .into_inner();
    (ops, samples)
}

/// Run K tokio tasks sharing one T5 generator for `window`. Returns (ops, samples).
async fn concurrent_t5_window(
    gen: Arc<T5TitleGenerator>,
    concurrency: usize,
    window: Duration,
) -> (u64, Vec<Duration>) {
    let total_ops = Arc::new(AtomicU64::new(0));
    let all_samples: Arc<tokio::sync::Mutex<Vec<Duration>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let deadline = Instant::now() + window;

    let mut handles = Vec::with_capacity(concurrency);
    for task_idx in 0..concurrency {
        let g = Arc::clone(&gen);
        let ops_counter = Arc::clone(&total_ops);
        let samples_out = Arc::clone(&all_samples);
        handles.push(tokio::spawn(async move {
            let mut local = Vec::new();
            let mut i = 0usize;
            while Instant::now() < deadline {
                let input = INPUTS[(task_idx + i) % INPUTS.len()];
                let t = Instant::now();
                if g.generate(input).await.is_ok() {
                    local.push(t.elapsed());
                    ops_counter.fetch_add(1, Ordering::Relaxed);
                }
                i += 1;
            }
            let mut guard = samples_out.lock().await;
            guard.extend(local);
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    let ops = total_ops.load(Ordering::Relaxed);
    let samples = Arc::try_unwrap(all_samples)
        .unwrap_or_else(|a| tokio::sync::Mutex::new(a.blocking_lock().clone()))
        .into_inner();
    (ops, samples)
}

// ---------------------------------------------------------------------------
// Lever E main bench: embedding + NLI + T5 concurrency tables
// ---------------------------------------------------------------------------

/// Lever E: single-session (current arch) vs naive pool-of-N, at K=1,2,4,8.
/// Covers all three ONNX model families used in the create+query hot path.
async fn bench_lever_e() {
    let intra = engramdb::onnx_ep::intra_threads();
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    println!(
        "\n=== Lever E: concurrent submission under multi-tenant daemon (corrected model) ===\
         \n    machine: {cores} cores | intra_threads (NLI/T5)={intra} | fastembed manages its own pool\
         \n    NOTE: all three models are now default-on (embedding + NLI + T5) in the multi-tenant daemon.\
         \n          Each shares ONE Arc<Mutex<Session>>. K = concurrent agent sessions inside a model call.\
         \n    create hot path: embedding -> T5 title -> NLI contradiction check (3 sequential Mutex acquires)"
    );

    // --- Embedding ---
    println!("\n  [embedding: all-MiniLM-Q int8 — every create + query]");
    let single_session = match OnnxProvider::with_model(engramdb::embeddings::ONNX_ALL_MINILM_Q) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            println!("  embedding model unavailable, skipping: {e}");
            return;
        }
    };
    // Pool-of-2: two independent OnnxProvider sessions, requests round-robin.
    let pool: Vec<Arc<OnnxProvider>> = (0..2)
        .filter_map(|_| {
            OnnxProvider::with_model(engramdb::embeddings::ONNX_ALL_MINILM_Q)
                .ok()
                .map(Arc::new)
        })
        .collect();
    // Warm up.
    let _ = single_session.embed(INPUTS[0]).await;
    for p in &pool {
        let _ = p.embed(INPUTS[0]).await;
    }

    println!(
        "\n  {:>3}  {:>14}  {:>9}  {:>9}  |  {:>14}  {:>9}  {:>9}",
        "K", "single ops/s", "mean ms", "p99 ms", "pool-2 ops/s", "mean ms", "p99 ms"
    );
    println!("  {}", "-".repeat(80));

    for &k in &[1usize, 2, 4, 8] {
        let (s_ops, s_samples) =
            concurrent_window(Arc::clone(&single_session), k, CONCURRENT_WINDOW).await;
        let s_stats = summarize(s_samples);
        let s_throughput = s_ops as f64 / CONCURRENT_WINDOW.as_secs_f64();

        let (p_ops, p_samples) = if pool.len() >= 2 {
            let idx = Arc::new(AtomicU64::new(0));
            let p_total_ops = Arc::new(AtomicU64::new(0));
            let p_all_samples: Arc<tokio::sync::Mutex<Vec<Duration>>> =
                Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let deadline = Instant::now() + CONCURRENT_WINDOW;
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
        let p_throughput = p_ops as f64 / CONCURRENT_WINDOW.as_secs_f64();

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

    println!(
        "\n  Oversubscription point: {cores} cores / {intra} intra_threads = {} parallel ORT sessions\
         \n  before threads compete. Pool-of-2 is safe iff intra_threads <= cores/2.",
        cores / intra.max(1)
    );

    // --- NLI ---
    println!("\n  [NLI: DeBERTa-v3-xsmall int8 — every create (contradiction check)]");
    let nli_single = match OnnxNliProvider::with_spec_on(&NLI_DEBERTA_XSMALL_Q, Backend::Cpu) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            println!("  NLI model unavailable, skipping: {e}");
            // still try T5
            bench_lever_e_t5(intra, cores).await;
            return;
        }
    };
    // Warm up NLI.
    let _ = nli_single.classify(INPUTS[0], INPUTS[1]).await;

    println!(
        "\n  {:>3}  {:>14}  {:>9}  {:>9}  (single-session; no pool — NLI is sequential within create)",
        "K", "single ops/s", "mean ms", "p99 ms",
    );
    println!("  {}", "-".repeat(65));

    for &k in &[1usize, 2, 4, 8] {
        let (ops, samples) =
            concurrent_nli_window(Arc::clone(&nli_single), k, CONCURRENT_WINDOW).await;
        let stats = summarize(samples);
        let throughput = ops as f64 / CONCURRENT_WINDOW.as_secs_f64();
        println!(
            "  {:>3}  {:>14.1}  {:>9.2}  {:>9.2}",
            k, throughput, stats.mean_ms, stats.p99_ms,
        );
    }

    // --- T5 ---
    bench_lever_e_t5(intra, cores).await;
}

async fn bench_lever_e_t5(intra: usize, cores: usize) {
    println!("\n  [T5: Xenova/t5-small int8 — every create (title generation)]");
    let t5_single = match T5TitleGenerator::new_on(Backend::Cpu) {
        Ok(g) => Arc::new(g),
        Err(e) => {
            println!("  T5 model unavailable, skipping: {e}");
            return;
        }
    };
    // Warm up T5.
    let _ = t5_single.generate(INPUTS[0]).await;

    println!(
        "\n  {:>3}  {:>14}  {:>9}  {:>9}  (single-session encoder+decoder Mutex pair)",
        "K", "single ops/s", "mean ms", "p99 ms",
    );
    println!("  {}", "-".repeat(65));

    for &k in &[1usize, 2, 4, 8] {
        let (ops, samples) =
            concurrent_t5_window(Arc::clone(&t5_single), k, CONCURRENT_WINDOW).await;
        let stats = summarize(samples);
        let throughput = ops as f64 / CONCURRENT_WINDOW.as_secs_f64();
        println!(
            "  {:>3}  {:>14.1}  {:>9.2}  {:>9.2}",
            k, throughput, stats.mean_ms, stats.p99_ms,
        );
    }

    println!(
        "\n  Interpretation for multi-tenant daemon (create path):\
         \n  - K agents each calling `create` concurrently → embedding + T5 + NLI all serialize.\
         \n  - Throughput plateau (K=1..8 ~flat): Mutex is the bottleneck for the model.\
         \n  - Per-agent p99 scales linearly with K (each waits for all predecessors).\
         \n  - NLI pool: single-session is already fast (~13ms); pooling buys ~54%% at K=2\
         \n    but requires reduced intra_threads to avoid oversubscription.\
         \n  - T5 pool: heavier (~85ms); same constraint — pool-2 needs intra_threads<=cores/2.\
         \n  - Oversubscription point: {cores} cores / {intra} intra_threads = {} sessions.",
        cores / intra.max(1)
    );
}

// ---------------------------------------------------------------------------
// Part B: prepacked_weights A/B
// ---------------------------------------------------------------------------

/// Bench NLI and T5 warm latency and concurrency with vs without
/// `PrepackedWeights`. Both variants are built from raw ort directly so the
/// A/B is self-contained in one bench run. Verdict (recorded for posterity):
/// **NO-GO** — measured 0–0.8% delta (noise) on Apple Silicon int8 at
/// intra_threads=4, so production (src/nli/onnx.rs, src/title/t5.rs) does
/// NOT ship prepacked. This harness is retained as the evidence.
async fn bench_prepacked_ab() {
    use ort::session::builder::GraphOptimizationLevel;
    use ort::session::builder::PrepackedWeights;
    use ort::session::Session;

    let intra = engramdb::onnx_ep::intra_threads();
    println!(
        "\n=== Part B: PrepackedWeights A/B (NLI + T5, intra_threads={intra}) ===\
         \n    Measures warm latency and K-concurrency with vs without prepacked weight matrices.\
         \n    PrepackedWeights pre-packs int8 GEMM weight matrices at session-init time,\
         \n    amortizing the packing cost over all inference calls. Expected 5-15%% warm gain\
         \n    did NOT materialize (measured ~0%); production does NOT ship prepacked."
    );

    // Build NLI "without" and "with" prepacked using raw ort session builder
    // directly (production wrappers ship WITHOUT prepacked — this is the A/B
    // that proved it). We use the int8 model (the production default) for both.
    let nli_model_path = {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
            .join("engramdb")
            .join("models");
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir)
            .build()
            .expect("hf api");
        let repo = api.model(NLI_DEBERTA_XSMALL_Q.repo.to_string());
        repo.get(NLI_DEBERTA_XSMALL_Q.model_file)
            .expect("nli model")
    };
    let nli_tokenizer_path = {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
            .join("engramdb")
            .join("models");
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir)
            .build()
            .expect("hf api");
        let repo = api.model(NLI_DEBERTA_XSMALL_Q.repo.to_string());
        repo.get(NLI_DEBERTA_XSMALL_Q.tokenizer_file)
            .expect("nli tokenizer")
    };

    // NLI session WITHOUT prepacked weights (legacy path).
    let nli_without = {
        let session = Session::builder()
            .expect("session builder")
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .expect("opt level")
            .with_intra_threads(intra)
            .expect("intra threads")
            .commit_from_file(&nli_model_path)
            .expect("nli session without prepacked");
        let tokenizer =
            tokenizers::Tokenizer::from_file(&nli_tokenizer_path).expect("nli tokenizer");
        (Arc::new(Mutex::new(session)), Arc::new(tokenizer))
    };

    // NLI session WITH prepacked weights (new path — mirrors src/nli/onnx.rs).
    let nli_with = {
        let weights = PrepackedWeights::new();
        let session = Session::builder()
            .expect("session builder")
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .expect("opt level")
            .with_intra_threads(intra)
            .expect("intra threads")
            .with_prepacked_weights(&weights)
            .expect("prepacked weights")
            .commit_from_file(&nli_model_path)
            .expect("nli session with prepacked");
        let tokenizer =
            tokenizers::Tokenizer::from_file(&nli_tokenizer_path).expect("nli tokenizer");
        (Arc::new(Mutex::new(session)), Arc::new(tokenizer))
    };

    println!("\n  [NLI warm latency: WITHOUT vs WITH prepacked_weights]");
    println!(
        "  {:>12}  {:>10}  {:>10}  {:>10}  {:>10}",
        "variant", "mean ms", "p50 ms", "p95 ms", "p99 ms"
    );
    println!("  {}", "-".repeat(58));

    for (label, (session, tokenizer)) in [("without", &nli_without), ("with", &nli_with)] {
        // Warm up.
        for _ in 0..5 {
            let mut s = session.lock().unwrap();
            let enc = tokenizer.encode(("premise", "hypothesis"), true).unwrap();
            let len = enc.len();
            let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
            let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
            let ids_t =
                ort::value::TensorRef::from_array_view(([1usize, len], ids.as_slice())).unwrap();
            let mask_t =
                ort::value::TensorRef::from_array_view(([1usize, len], mask.as_slice())).unwrap();
            let inputs: Vec<(Cow<str>, ort::session::SessionInputValue)> = vec![
                (Cow::Borrowed("input_ids"), ids_t.into()),
                (Cow::Borrowed("attention_mask"), mask_t.into()),
            ];
            let _ = s.run(inputs);
        }

        let mut samples = Vec::new();
        for i in 0..80 {
            let premise = INPUTS[i % INPUTS.len()];
            let hypothesis = INPUTS[(i + 1) % INPUTS.len()];
            let t = Instant::now();
            {
                let mut s = session.lock().unwrap();
                let enc = tokenizer.encode((premise, hypothesis), true).unwrap();
                let len = enc.len();
                let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
                let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
                let ids_t = ort::value::TensorRef::from_array_view(([1usize, len], ids.as_slice()))
                    .unwrap();
                let mask_t =
                    ort::value::TensorRef::from_array_view(([1usize, len], mask.as_slice()))
                        .unwrap();
                let inputs: Vec<(Cow<str>, ort::session::SessionInputValue)> = vec![
                    (Cow::Borrowed("input_ids"), ids_t.into()),
                    (Cow::Borrowed("attention_mask"), mask_t.into()),
                ];
                let _ = s.run(inputs);
            }
            samples.push(t.elapsed());
        }
        let stats = summarize(samples);
        println!(
            "  {:>12}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}",
            label, stats.mean_ms, stats.p50_ms, stats.p95_ms, stats.p99_ms
        );
    }

    // NLI under K-concurrency: raw Mutex sessions to isolate prepacked effect.
    println!("\n  [NLI K-concurrency: WITHOUT vs WITH prepacked_weights, warm mean ms / p99 ms]");
    println!(
        "  {:>3}  {:>12}  {:>10}  |  {:>12}  {:>10}",
        "K", "w/o mean ms", "w/o p99 ms", "with mean ms", "with p99 ms"
    );
    println!("  {}", "-".repeat(58));

    for &k in &[1usize, 2, 4] {
        async fn run_concurrent_nli_raw(
            session: Arc<Mutex<ort::session::Session>>,
            tokenizer: Arc<tokenizers::Tokenizer>,
            k: usize,
            window: Duration,
        ) -> Vec<Duration> {
            let all_samples: Arc<tokio::sync::Mutex<Vec<Duration>>> =
                Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let deadline = Instant::now() + window;
            let mut handles = Vec::with_capacity(k);
            for task_idx in 0..k {
                let s = Arc::clone(&session);
                let tok = Arc::clone(&tokenizer);
                let out = Arc::clone(&all_samples);
                handles.push(tokio::spawn(async move {
                    let mut local = Vec::new();
                    let mut i = 0usize;
                    while Instant::now() < deadline {
                        let premise = INPUTS[(task_idx + i) % INPUTS.len()];
                        let hypothesis = INPUTS[(task_idx + i + 1) % INPUTS.len()];
                        let t = Instant::now();
                        let session_clone = Arc::clone(&s);
                        let tok_clone = Arc::clone(&tok);
                        let premise_s = premise.to_string();
                        let hypothesis_s = hypothesis.to_string();
                        let ok = tokio::task::spawn_blocking(move || {
                            let mut s = session_clone.lock().unwrap();
                            let enc = tok_clone
                                .encode((premise_s.as_str(), hypothesis_s.as_str()), true)
                                .unwrap();
                            let len = enc.len();
                            let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
                            let mask: Vec<i64> =
                                enc.get_attention_mask().iter().map(|&x| x as i64).collect();
                            let ids_t = ort::value::TensorRef::from_array_view((
                                [1usize, len],
                                ids.as_slice(),
                            ))
                            .unwrap();
                            let mask_t = ort::value::TensorRef::from_array_view((
                                [1usize, len],
                                mask.as_slice(),
                            ))
                            .unwrap();
                            let inputs: Vec<(Cow<str>, ort::session::SessionInputValue)> = vec![
                                (Cow::Borrowed("input_ids"), ids_t.into()),
                                (Cow::Borrowed("attention_mask"), mask_t.into()),
                            ];
                            let ok = s.run(inputs).is_ok();
                            ok
                        })
                        .await
                        .unwrap_or(false);
                        if ok {
                            local.push(t.elapsed());
                        }
                        i += 1;
                    }
                    let mut g = out.lock().await;
                    g.extend(local);
                }));
            }
            for h in handles {
                let _ = h.await;
            }
            Arc::try_unwrap(all_samples)
                .unwrap_or_else(|a| tokio::sync::Mutex::new(a.blocking_lock().clone()))
                .into_inner()
        }

        let wo_samples = run_concurrent_nli_raw(
            Arc::clone(&nli_without.0),
            Arc::clone(&nli_without.1),
            k,
            CONCURRENT_WINDOW,
        )
        .await;
        let wi_samples = run_concurrent_nli_raw(
            Arc::clone(&nli_with.0),
            Arc::clone(&nli_with.1),
            k,
            CONCURRENT_WINDOW,
        )
        .await;
        let wo = summarize(wo_samples);
        let wi = summarize(wi_samples);
        println!(
            "  {:>3}  {:>12.2}  {:>10.2}  |  {:>12.2}  {:>10.2}",
            k, wo.mean_ms, wo.p99_ms, wi.mean_ms, wi.p99_ms
        );
    }

    // T5 A/B: use the production wrappers (OnnxNliProvider and T5TitleGenerator)
    // since both now always use prepacked. Instead, A/B by building two T5
    // generators via raw ort for "without" vs our wrapper for "with".
    println!("\n  [T5 warm latency: WITHOUT vs WITH prepacked_weights]");

    let (enc_path, dec_path, tok_path) = {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
            .join("engramdb")
            .join("models");
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir)
            .build()
            .expect("hf api");
        let spec = &engramdb::title::t5::T5_XENOVA_Q;
        let repo = api.model(spec.repo.to_string());
        (
            repo.get(spec.encoder_file).expect("encoder"),
            repo.get(spec.decoder_file).expect("decoder"),
            repo.get(spec.tokenizer_file).expect("tokenizer"),
        )
    };

    let t5_tok = Arc::new(tokenizers::Tokenizer::from_file(&tok_path).expect("t5 tokenizer"));

    // T5 WITHOUT prepacked.
    let t5_enc_without = Arc::new(Mutex::new(
        Session::builder()
            .expect("sb")
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .expect("opt")
            .with_intra_threads(intra)
            .expect("intra")
            .commit_from_file(&enc_path)
            .expect("enc without prepacked"),
    ));
    let t5_dec_without = Arc::new(Mutex::new(
        Session::builder()
            .expect("sb")
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .expect("opt")
            .with_intra_threads(intra)
            .expect("intra")
            .commit_from_file(&dec_path)
            .expect("dec without prepacked"),
    ));

    // T5 WITH prepacked (mirrors src/title/t5.rs).
    let enc_weights = PrepackedWeights::new();
    let t5_enc_with = Arc::new(Mutex::new(
        Session::builder()
            .expect("sb")
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .expect("opt")
            .with_intra_threads(intra)
            .expect("intra")
            .with_prepacked_weights(&enc_weights)
            .expect("prepacked")
            .commit_from_file(&enc_path)
            .expect("enc with prepacked"),
    ));
    let dec_weights = PrepackedWeights::new();
    let t5_dec_with = Arc::new(Mutex::new(
        Session::builder()
            .expect("sb")
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .expect("opt")
            .with_intra_threads(intra)
            .expect("intra")
            .with_prepacked_weights(&dec_weights)
            .expect("prepacked")
            .commit_from_file(&dec_path)
            .expect("dec with prepacked"),
    ));

    println!(
        "  {:>12}  {:>10}  {:>10}  {:>10}  {:>10}",
        "variant", "mean ms", "p50 ms", "p95 ms", "p99 ms"
    );
    println!("  {}", "-".repeat(58));

    for (label, enc, dec) in [
        (
            "without",
            Arc::clone(&t5_enc_without),
            Arc::clone(&t5_dec_without),
        ),
        ("with", Arc::clone(&t5_enc_with), Arc::clone(&t5_dec_with)),
    ] {
        // Warm-up runs (T5 is slow — 5 warmup iters sufficient).
        for _ in 0..3 {
            run_t5_inference_raw(&enc, &dec, &t5_tok, INPUTS[0]);
        }
        let mut samples = Vec::new();
        for i in 0..24 {
            let input = INPUTS[i % INPUTS.len()];
            let t = Instant::now();
            run_t5_inference_raw(&enc, &dec, &t5_tok, input);
            samples.push(t.elapsed());
        }
        let stats = summarize(samples);
        println!(
            "  {:>12}  {:>10.2}  {:>10.2}  {:>10.2}  {:>10.2}",
            label, stats.mean_ms, stats.p50_ms, stats.p95_ms, stats.p99_ms
        );
    }

    println!("\n  [T5 K-concurrency: WITHOUT vs WITH prepacked_weights, warm mean ms / p99 ms]");
    println!(
        "  {:>3}  {:>12}  {:>10}  |  {:>12}  {:>10}",
        "K", "w/o mean ms", "w/o p99 ms", "with mean ms", "with p99 ms"
    );
    println!("  {}", "-".repeat(58));

    for &k in &[1usize, 2, 4] {
        let wo_samples = run_concurrent_t5_raw(
            Arc::clone(&t5_enc_without),
            Arc::clone(&t5_dec_without),
            Arc::clone(&t5_tok),
            k,
            CONCURRENT_WINDOW,
        )
        .await;
        let wi_samples = run_concurrent_t5_raw(
            Arc::clone(&t5_enc_with),
            Arc::clone(&t5_dec_with),
            Arc::clone(&t5_tok),
            k,
            CONCURRENT_WINDOW,
        )
        .await;
        let wo = summarize(wo_samples);
        let wi = summarize(wi_samples);
        println!(
            "  {:>3}  {:>12.2}  {:>10.2}  |  {:>12.2}  {:>10.2}",
            k, wo.mean_ms, wo.p99_ms, wi.mean_ms, wi.p99_ms
        );
    }
}

/// Synchronous T5 inference (encoder + greedy decode, max 8 output tokens)
/// operating directly on raw ort `Session` Mutexes. Used for the prepacked A/B
/// without going through T5TitleGenerator.
fn run_t5_inference_raw(
    encoder: &Mutex<ort::session::Session>,
    decoder: &Mutex<ort::session::Session>,
    tokenizer: &tokenizers::Tokenizer,
    text: &str,
) {
    let input = format!("summarize: {}", text);
    let enc_result = tokenizer.encode(input.as_str(), true).unwrap();
    let ids: Vec<i64> = enc_result
        .get_ids()
        .iter()
        .take(64)
        .map(|&x| x as i64)
        .collect();
    let mask: Vec<i64> = enc_result
        .get_attention_mask()
        .iter()
        .take(64)
        .map(|&x| x as i64)
        .collect();
    let length = ids.len();

    let hidden = {
        let ids_t =
            ort::value::TensorRef::from_array_view(([1usize, length], ids.as_slice())).unwrap();
        let mask_t =
            ort::value::TensorRef::from_array_view(([1usize, length], mask.as_slice())).unwrap();
        let mut enc = encoder.lock().unwrap();
        let enc_inputs: Vec<(Cow<str>, ort::session::SessionInputValue)> = vec![
            (Cow::Borrowed("input_ids"), ids_t.into()),
            (Cow::Borrowed("attention_mask"), mask_t.into()),
        ];
        let outputs = enc.run(enc_inputs).unwrap();
        let (_, data) = outputs[0].try_extract_tensor::<f32>().unwrap();
        let v = data.to_vec();
        let hidden_dim = v.len() / length.max(1);
        (v, vec![1usize, length, hidden_dim])
    };

    // Greedy decode (max 8 tokens for speed in bench).
    let mut generated: Vec<i64> = vec![0];
    for _ in 0..8 {
        let dec_len = generated.len();
        let dec_t =
            ort::value::TensorRef::from_array_view(([1usize, dec_len], generated.as_slice()))
                .unwrap();
        let enc_t = ort::value::TensorRef::from_array_view((hidden.1.clone(), hidden.0.as_slice()))
            .unwrap();
        let enc_mask = vec![1i64; hidden.1[1]];
        let enc_mask_t =
            ort::value::TensorRef::from_array_view(([1usize, hidden.1[1]], enc_mask.as_slice()))
                .unwrap();
        let mut dec = decoder.lock().unwrap();
        let dec_inputs: Vec<(Cow<str>, ort::session::SessionInputValue)> = vec![
            (Cow::Borrowed("input_ids"), dec_t.into()),
            (Cow::Borrowed("encoder_hidden_states"), enc_t.into()),
            (Cow::Borrowed("encoder_attention_mask"), enc_mask_t.into()),
        ];
        let outputs = dec.run(dec_inputs).unwrap();
        let (_, logits) = outputs[0].try_extract_tensor::<f32>().unwrap();
        let vocab = logits.len() / dec_len;
        let last = &logits[(dec_len - 1) * vocab..dec_len * vocab];
        let next = last
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i as i64)
            .unwrap_or(1);
        if next == 1 {
            break;
        }
        generated.push(next);
    }
}

async fn run_concurrent_t5_raw(
    encoder: Arc<Mutex<ort::session::Session>>,
    decoder: Arc<Mutex<ort::session::Session>>,
    tokenizer: Arc<tokenizers::Tokenizer>,
    k: usize,
    window: Duration,
) -> Vec<Duration> {
    let all_samples: Arc<tokio::sync::Mutex<Vec<Duration>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let deadline = Instant::now() + window;
    let mut handles = Vec::with_capacity(k);
    for task_idx in 0..k {
        let enc = Arc::clone(&encoder);
        let dec = Arc::clone(&decoder);
        let tok = Arc::clone(&tokenizer);
        let out = Arc::clone(&all_samples);
        handles.push(tokio::spawn(async move {
            let mut local = Vec::new();
            let mut i = 0usize;
            while Instant::now() < deadline {
                let input_str = INPUTS[(task_idx + i) % INPUTS.len()].to_string();
                let enc2 = Arc::clone(&enc);
                let dec2 = Arc::clone(&dec);
                let tok2 = Arc::clone(&tok);
                let t = Instant::now();
                let ok = tokio::task::spawn_blocking(move || {
                    // Catch any panic from the raw inference (e.g. under oversubscription).
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_t5_inference_raw(&enc2, &dec2, &tok2, &input_str);
                    }))
                    .is_ok()
                })
                .await
                .unwrap_or(false);
                if ok {
                    local.push(t.elapsed());
                }
                i += 1;
            }
            let mut g = out.lock().await;
            g.extend(local);
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    Arc::try_unwrap(all_samples)
        .unwrap_or_else(|a| tokio::sync::Mutex::new(a.blocking_lock().clone()))
        .into_inner()
}

// ---------------------------------------------------------------------------
// Per-backend suite (unchanged from prior version)
// ---------------------------------------------------------------------------

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

    if enabled("prepacked_ab") {
        bench_prepacked_ab().await;
    }

    // Skip the per-backend suite when only lever_e / prepacked_ab was requested.
    let only_focused = std::env::var("ENGRAMDB_BENCH_WORKLOADS")
        .map(|v| {
            v.split(',')
                .all(|w| matches!(w.trim(), "lever_e" | "prepacked_ab"))
        })
        .unwrap_or(false);
    if !only_focused {
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
