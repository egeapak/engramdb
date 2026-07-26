//! One table: every model in the stack × every action it performs.
//!
//! The other benches each answer one question (which embedding model ranks
//! best, which reranker is cheaper). This one exists to tune *defaults*: it puts
//! the cost of every model and every operation side by side, so the question
//! "what does turning this on actually cost per query / per create" has a single
//! answer to read off.
//!
//! Actions measured, per model:
//! - **load** — cold construction, i.e. what a daemon-less CLI pays once.
//! - the model's real per-call work: one query embed, a batch embed, one
//!   query→document rerank pair, one premise/hypothesis NLI pair, one title
//!   generation.
//!
//! Reported as mean / p50 / p95 over N warm iterations, plus the per-unit cost
//! (ms per text, ms per pair).
//!
//! **Read `per_unit` within a row, not across rows.** "embed 1 query" uses a
//! real query (a handful of tokens); "embed batch of 16" uses real content
//! chunks at the full 256-token budget. The batch row therefore costs more per
//! text — that is sequence length, not batching overhead. It is the honest
//! comparison for capacity planning (a query really is short, a chunk really is
//! long) but it is not a batching efficiency measurement.
//!
//! `load (cold)` is session construction with the weights **already cached**.
//! Run it twice on a fresh machine: the first run's load figures include the
//! HuggingFace download and are not comparable.
//!
//! Run: `cargo run --release --example model_cost_matrix`
//! Env: `COST_ITERS` (default 30), `COST_MODELS` (comma filter over row labels),
//!      `EMBED_EVAL_DATA` (corpus for realistic inputs).

use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{
    chunk_text, EmbeddingProvider, OnnxModelSpec, OnnxProvider, ONNX_ALL_MINILM,
    ONNX_ALL_MINILM_L12, ONNX_ALL_MINILM_L12_Q, ONNX_ALL_MINILM_L12_U8, ONNX_ALL_MINILM_L6_U8,
    ONNX_ALL_MINILM_Q, ONNX_ARCTIC_XS_Q,
};
use engramdb::nli::{NliProvider, OnnxNliProvider, DEFAULT_NLI_MODEL, NLI_DEBERTA_XSMALL};
use engramdb::onnx_ep::Backend;
use engramdb::retrieval::reranker::LocalReranker;
use engramdb::title::t5::{T5TitleGenerator, T5_OPTIMUM, T5_XENOVA_Q};
use engramdb::title::{TitleGenerator, TitleStrategy};
use serde::Deserialize;

const CHUNK_BUDGET_TOKENS: usize = 256;
const BATCH: usize = 16;

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

struct Row {
    role: &'static str,
    model: String,
    action: String,
    /// Per-call cost. For batch rows this is the whole batch.
    mean: f64,
    p50: f64,
    p95: f64,
    /// Cost attributed to one unit of work (one text, one pair). Equals `mean`
    /// for single-item actions.
    per_unit: f64,
    unit: &'static str,
}

fn stats(mut samples: Vec<f64>, units_per_call: usize) -> (f64, f64, f64, f64) {
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    (
        mean,
        at(0.50),
        at(0.95),
        mean / units_per_call.max(1) as f64,
    )
}

async fn time_async<F, Fut, T>(iters: usize, mut f: F) -> Result<Vec<f64>>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut out = Vec::with_capacity(iters);
    for i in 0..iters {
        let t = Instant::now();
        f(i).await?;
        out.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(out)
}

async fn embedding_rows(
    label: &'static str,
    spec: OnnxModelSpec,
    docs: &[String],
    queries: &[String],
    iters: usize,
    rows: &mut Vec<Row>,
) -> Result<()> {
    let t0 = Instant::now();
    let Ok(provider) = OnnxProvider::with_model(spec) else {
        println!("  (skipped {label}: model unavailable)");
        return Ok(());
    };
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let id = provider.model_id();
    rows.push(Row {
        role: "embedding",
        model: id.clone(),
        action: "load (cold)".into(),
        mean: load_ms,
        p50: load_ms,
        p95: load_ms,
        per_unit: load_ms,
        unit: "once",
    });

    for q in queries.iter().take(3) {
        provider.embed(q).await?;
    }
    let s = time_async(iters, |i| provider.embed(&queries[i % queries.len()])).await?;
    let (m, p50, p95, pu) = stats(s, 1);
    rows.push(Row {
        role: "embedding",
        model: id.clone(),
        action: "embed 1 query".into(),
        mean: m,
        p50,
        p95,
        per_unit: pu,
        unit: "text",
    });

    let p = &provider;
    let s = time_async(iters.max(8) / 4, |i| {
        let start = (i * BATCH) % docs.len().saturating_sub(BATCH).max(1);
        let slice: Vec<String> = docs[start..(start + BATCH).min(docs.len())].to_vec();
        async move {
            let refs: Vec<&str> = slice.iter().map(|s| s.as_str()).collect();
            p.embed_batch(&refs).await.map(|_| ())
        }
    })
    .await?;
    let (m, p50, p95, pu) = stats(s, BATCH);
    rows.push(Row {
        role: "embedding",
        model: id,
        action: format!("embed batch of {BATCH}"),
        mean: m,
        p50,
        p95,
        per_unit: pu,
        unit: "text",
    });
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_path =
        std::env::var("EMBED_EVAL_DATA").unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let iters: usize = std::env::var("COST_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let filter: Option<Vec<String>> = std::env::var("COST_MODELS")
        .ok()
        .map(|s| s.split(',').map(|m| m.trim().to_string()).collect());
    let want = |k: &str| {
        filter
            .as_ref()
            .is_none_or(|f| f.iter().any(|m| k.contains(m.as_str())))
    };

    let raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("reading dataset {data_path}"))?;
    let ds: Dataset = serde_json::from_str(&raw).context("parsing dataset")?;
    let mut docs: Vec<String> = ds
        .memories
        .iter()
        .flat_map(|m| chunk_text(&format!("{} {}", m.summary, m.content), CHUNK_BUDGET_TOKENS))
        .collect();
    docs.sort();
    let queries: Vec<String> = ds.queries.iter().map(|q| q.text.clone()).collect();
    // Rerank documents mirror `retrieval::engine::rerank_document`.
    let rerank_docs: Vec<String> = ds
        .memories
        .iter()
        .map(|m| format!("{}. {} {}", m.title, m.summary, m.content))
        .collect();

    let mut rows: Vec<Row> = Vec::new();

    println!("measuring embeddings…");
    for (label, spec) in [
        ("minilm-l12-u8 (default)", ONNX_ALL_MINILM_L12_U8),
        ("minilm-l6-u8", ONNX_ALL_MINILM_L6_U8),
        ("minilm-l12-int8", ONNX_ALL_MINILM_L12_Q),
        ("minilm-l6-int8", ONNX_ALL_MINILM_Q),
        ("minilm-l12-fp32", ONNX_ALL_MINILM_L12),
        ("minilm-l6-fp32", ONNX_ALL_MINILM),
        ("arctic-xs-int8", ONNX_ARCTIC_XS_Q),
    ] {
        if want(label) {
            embedding_rows(label, spec, &docs, &queries, iters, &mut rows).await?;
        }
    }

    println!("measuring rerankers…");
    for name in ["jina-reranker-v1-turbo-en", "bge-reranker-base"] {
        if !want(name) {
            continue;
        }
        let t0 = Instant::now();
        let Ok(reranker) = LocalReranker::load(name) else {
            println!("  (skipped {name}: model unavailable)");
            continue;
        };
        let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(Row {
            role: "reranker",
            model: name.to_string(),
            action: "load (cold)".into(),
            mean: load_ms,
            p50: load_ms,
            p95: load_ms,
            per_unit: load_ms,
            unit: "once",
        });
        // One pair at a time isolates per-pair cost; `rerank.top_n` multiplies it.
        let one = rerank_docs[..1].to_vec();
        let s = time_async(iters.min(12), |i| {
            let q = queries[i % queries.len()].clone();
            let d = one.clone();
            let r = &reranker;
            async move { r.rerank(&q, &d).await.map(|_| ()) }
        })
        .await?;
        let (m, p50, p95, pu) = stats(s, 1);
        rows.push(Row {
            role: "reranker",
            model: name.to_string(),
            action: "score 1 pair".into(),
            mean: m,
            p50,
            p95,
            per_unit: pu,
            unit: "pair",
        });
    }

    println!("measuring NLI…");
    for (label, spec) in [
        ("nli-deberta-v3-xsmall-q (default)", DEFAULT_NLI_MODEL),
        ("nli-deberta-v3-xsmall-fp32", NLI_DEBERTA_XSMALL),
    ] {
        if !want(label) {
            continue;
        }
        let t0 = Instant::now();
        let Some(nli) = OnnxNliProvider::try_with_spec_on(&spec, Backend::Cpu) else {
            println!("  (skipped {label}: model unavailable)");
            continue;
        };
        let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(Row {
            role: "nli",
            model: label.to_string(),
            action: "load (cold)".into(),
            mean: load_ms,
            p50: load_ms,
            p95: load_ms,
            per_unit: load_ms,
            unit: "once",
        });
        let s = time_async(iters.min(20), |i| {
            let a = rerank_docs[i % rerank_docs.len()].clone();
            let b = rerank_docs[(i + 1) % rerank_docs.len()].clone();
            let n = &nli;
            async move { n.classify(&a, &b).await.map(|_| ()) }
        })
        .await?;
        let (m, p50, p95, pu) = stats(s, 1);
        rows.push(Row {
            role: "nli",
            model: label.to_string(),
            action: "1 premise/hypothesis pair".into(),
            mean: m,
            p50,
            p95,
            per_unit: pu,
            unit: "pair",
        });
    }

    println!("measuring title generation…");
    for (label, spec) in [
        ("t5-small-q (default)", T5_XENOVA_Q),
        ("t5-small-fp32", T5_OPTIMUM),
    ] {
        if !want(label) {
            continue;
        }
        let t0 = Instant::now();
        let Ok(t5) = T5TitleGenerator::with_spec(&spec) else {
            println!("  (skipped {label}: model unavailable)");
            continue;
        };
        let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(Row {
            role: "title",
            model: label.to_string(),
            action: "load (cold)".into(),
            mean: load_ms,
            p50: load_ms,
            p95: load_ms,
            per_unit: load_ms,
            unit: "once",
        });
        let s = time_async(iters.min(10), |i| {
            let text = rerank_docs[i % rerank_docs.len()].clone();
            let g = &t5;
            async move { g.generate(&text).await.map(|_| ()) }
        })
        .await?;
        let (m, p50, p95, pu) = stats(s, 1);
        rows.push(Row {
            role: "title",
            model: label.to_string(),
            action: "generate 1 title".into(),
            mean: m,
            p50,
            p95,
            per_unit: pu,
            unit: "title",
        });
    }

    // Keyword titling is the model-free alternative; worth a row so the T5
    // default has something to be compared against.
    if want("keyword") {
        let s = time_async(iters, |i| {
            let text = rerank_docs[i % rerank_docs.len()].clone();
            async move { Ok(engramdb::title::generate_title(TitleStrategy::Keyword, &text).await) }
        })
        .await?;
        let (m, p50, p95, pu) = stats(s, 1);
        rows.push(Row {
            role: "title",
            model: "keyword (RAKE, no model)".into(),
            action: "generate 1 title".into(),
            mean: m,
            p50,
            p95,
            per_unit: pu,
            unit: "title",
        });
    }

    println!(
        "\n{:<10} {:<34} {:<24} {:>9} {:>9} {:>9} {:>12}",
        "role", "model", "action", "mean_ms", "p50_ms", "p95_ms", "per_unit"
    );
    println!("{}", "-".repeat(112));
    for r in &rows {
        println!(
            "{:<10} {:<34} {:<24} {:>9.2} {:>9.2} {:>9.2} {:>9.2}/{}",
            r.role, r.model, r.action, r.mean, r.p50, r.p95, r.per_unit, r.unit
        );
    }
    Ok(())
}
