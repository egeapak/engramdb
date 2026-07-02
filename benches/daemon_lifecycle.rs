//! Daemon lifecycle bench: warm-daemon resolve vs in-process cold-load.
//!
//! Compares the latency of obtaining model-backed providers via two paths:
//!
//! - **in_process_cold**: `resolve_engine_providers(config, None, 1)` — the
//!   classic CLI path that loads ONNX in-process on every call (simulates a
//!   cold first-call because Criterion re-invokes the closure).
//! - **daemon_connect_only**: `resolve_providers(cell, config, None, dir,
//!   ConnectOnly)` against a pre-warmed daemon on a temp socket — quantifies
//!   the latency of the "connect, send Meta, receive providers" round-trip once
//!   the daemon is already running.
//!
//! **Guard:** if `OnnxProvider::try_new()` returns `None` (model not staged),
//! the bench prints a skip message and returns without registering any
//! Criterion measurements, so it never fails on CI when models are unavailable.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use tempfile::TempDir;
use tokio::runtime::Runtime;

use engramdb::daemon::{resolve_providers, DaemonCell, DaemonPolicy};
use engramdb::embeddings::OnnxProvider;
use engramdb::ops::resolve_engine_providers;
use engramdb::types::EngramConfig;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
}

/// Poll the socket path until a connection succeeds or the timeout elapses.
async fn poll_until_connectable(socket: &std::path::Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::UnixStream::connect(socket).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("daemon did not become connectable within 10s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Benchmark group
// ---------------------------------------------------------------------------

fn daemon_lifecycle_benchmarks(c: &mut Criterion) {
    // Guard: skip if model is not staged.
    if OnnxProvider::try_new().is_none() {
        eprintln!(
            "[daemon_lifecycle bench] SKIP — ONNX embedding model not staged. \
             Stage the quantized all-MiniLM-L6-v2 model to run this bench."
        );
        return;
    }

    let rt = runtime();

    // Spin up a shared daemon for the warm-path bench.
    let tmp = TempDir::new().expect("failed to create temp dir");
    let socket = tmp.path().join("bench.sock");

    rt.block_on(async {
        use engramdb::daemon::server::run_daemon;
        let sock = socket.clone();
        tokio::spawn(run_daemon(sock, Duration::from_secs(3600)));
        poll_until_connectable(&socket).await;
    });

    let mut config = EngramConfig::default();
    // Disable optional heavy models (NLI, reranker, T5) — we only benchmark
    // the embedding provider resolution path.
    config.nli.enabled = false;
    config.rerank.enabled = false;
    // Point the daemon field at our temp socket.
    config.daemon.enabled = true;
    config.daemon.socket_path = Some(socket.to_string_lossy().into_owned());
    let dir = tmp.path().to_path_buf();

    let mut group = c.benchmark_group("daemon_lifecycle");
    // Use a small sample count — model loading is slow and we want a
    // representative measurement without exhausting CI time budgets.
    group.sample_size(10);

    // -----------------------------------------------------------------------
    // Bench A: in-process cold load
    //
    // Each iteration invokes `resolve_engine_providers`, which loads the ONNX
    // model from the cache dir.  This simulates the current CLI behaviour.
    // -----------------------------------------------------------------------
    group.bench_function("in_process_cold", |b| {
        let cfg = config.clone();
        b.iter(|| {
            // Drop the result so the model is re-loaded on the next iteration.
            let _ = resolve_engine_providers(&cfg, None, 1);
        });
    });

    // -----------------------------------------------------------------------
    // Bench B: warm-daemon connect-only round-trip
    //
    // Each iteration connects to the already-running daemon, sends `Meta`,
    // and receives remote-backed providers.  This simulates the new CLI path
    // when a daemon is resident.
    // -----------------------------------------------------------------------
    group.bench_function("daemon_connect_only", |b| {
        let cfg = config.clone();
        let dir_ref = dir.clone();
        b.to_async(&rt).iter(|| async {
            let cell = DaemonCell::new();
            let _ = resolve_providers(&cell, &cfg, None, &dir_ref, DaemonPolicy::ConnectOnly).await;
        });
    });

    group.finish();
}

criterion_group!(benches, daemon_lifecycle_benchmarks);
criterion_main!(benches);
