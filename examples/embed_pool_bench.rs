//! Embedding-pool throughput through the production path.
//!
//! Builds the embedding pool exactly as the daemon / MCP server does
//! (`ops::resolve_engine_providers` with the configured pool size, so each
//! session gets `EmbeddingsConfig::session_intra_threads`), then drives it
//! with C concurrent callers, each sending one text per request, for a fixed
//! window. Reports requests/s and per-request p50 / p99 for short queries
//! and memory-length documents.
//!
//! Run with the ONNX embedding model present in the unified cache:
//!   cargo run --release --example embed_pool_bench
//!
//! Env: `POOL_SIZES` (default: auto `cores/2`, plus 1 for reference),
//! `POOL_CALLERS` (1,2,4,8), `POOL_WINDOW_SECS` (4), `EMBED_EVAL_DATA`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use engramdb::embeddings::EmbeddingProvider;
use engramdb::ops;
use engramdb::types::config::available_cores;
use engramdb::types::EngramConfig;
use serde::Deserialize;

#[derive(Deserialize)]
struct Dataset {
    memories: Vec<Mem>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Mem {
    title: String,
    summary: String,
    content: String,
}

#[derive(Deserialize)]
struct Query {
    text: String,
}

fn env_list(name: &str, default: Vec<usize>) -> Vec<usize> {
    std::env::var(name)
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or(default)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

async fn run_point(
    provider: Arc<dyn EmbeddingProvider>,
    texts: Arc<Vec<String>>,
    callers: usize,
    window: Duration,
) -> (f64, f64, f64) {
    let start = Instant::now();
    let deadline = start + window;
    let mut tasks = Vec::new();
    for c in 0..callers {
        let (provider, texts) = (provider.clone(), texts.clone());
        tasks.push(tokio::spawn(async move {
            let mut lat = Vec::new();
            let mut i = c * 7;
            while Instant::now() < deadline {
                let t = Instant::now();
                provider
                    .embed(&texts[i % texts.len()])
                    .await
                    .expect("embed");
                lat.push(t.elapsed().as_secs_f64() * 1e3);
                i += 1;
            }
            lat
        }));
    }
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.expect("caller task"));
    }
    let rps = all.len() as f64 / start.elapsed().as_secs_f64();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (rps, pct(&all, 0.50), pct(&all, 0.99))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let path =
        std::env::var("EMBED_EVAL_DATA").unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let ds: Dataset = serde_json::from_str(&std::fs::read_to_string(&path).expect("eval data"))
        .expect("parse eval data");
    let queries = Arc::new(ds.queries.into_iter().map(|q| q.text).collect::<Vec<_>>());
    let docs = Arc::new(
        ds.memories
            .into_iter()
            .map(|m| format!("{}. {} {}", m.title, m.summary, m.content))
            .collect::<Vec<_>>(),
    );

    let cores = available_cores();
    // Embeddings only: the reranker / NLI / T5 sessions are not pooled the
    // same way and would only add load time here.
    let mut config = EngramConfig::default();
    config.rerank.enabled = false;
    config.nli.enabled = false;
    config.title.strategy = engramdb::title::TitleStrategy::Keyword;
    let auto = config.embeddings.resolved_pool_size(cores);
    let mut pools = env_list("POOL_SIZES", vec![1, auto]);
    pools.dedup();
    let callers = env_list("POOL_CALLERS", vec![1, 2, 4, 8]);
    let window = Duration::from_secs_f64(
        std::env::var("POOL_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4.0),
    );
    println!("cores={cores}, auto pool={auto}, window={window:?}");
    println!("| pool | workload | callers | req/s | p50 ms | p99 ms |");
    println!("|---:|---|---:|---:|---:|---:|");
    for pool in pools {
        let providers = ops::resolve_engine_providers(&config, None, pool);
        let provider = providers.embedding.expect("embedding provider");
        provider.embed("warm up").await.expect("warm-up");
        for (name, texts) in [("query", &queries), ("doc", &docs)] {
            for &c in &callers {
                let (rps, p50, p99) = run_point(provider.clone(), texts.clone(), c, window).await;
                println!("| {pool} | {name} | {c} | {rps:.1} | {p50:.1} | {p99:.1} |");
            }
        }
    }
}
