//! Cross-encoder reranker A/B: quality and cost of each `[rerank].model`.
//!
//! Reranking is the most expensive thing EngramDB can be asked to load — the
//! historical `bge-reranker-base` default is a **1.1 GB** fp32 download — and
//! it is off by default, so its cost/benefit had never been measured on this
//! corpus. This harness measures it through the production seams:
//!
//! - candidates come from the real bi-encoder path (`OnnxProvider` at the
//!   shipped default, `chunk_text`, metadata vector + content chunks, max
//!   aggregation) — i.e. what `LanceIndex::vector_search` would rank;
//! - documents are composed exactly like `retrieval::engine::rerank_document`
//!   (`"{title}. {summary} {content} tags: …"`);
//! - scoring goes through the real [`LocalReranker`], and the blend mirrors
//!   `apply_rerank`: `(1 - weight) * base + weight * sigmoid(logit)`.
//!
//! Reported per model: load time, ms/pair, ms/query at the configured `top_n`,
//! and P@1 / R@5 / MRR@10 / nDCG@10 against the graded corpus — plus the
//! no-rerank baseline, so "is reranking worth enabling at all" is answerable.
//!
//! Run: `cargo run --release --example rerank_bench`
//! Env: `RERANK_BENCH_MODELS` (comma filter), `RERANK_BENCH_TOP_N` (default 20),
//!      `RERANK_BENCH_WEIGHT` (default 0.5), `EMBED_EVAL_DATA` (corpus).

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{chunk_text, EmbeddingProvider, OnnxProvider};
use engramdb::retrieval::reranker::LocalReranker;
use engramdb::types::DEFAULT_RERANK_MODEL;
use serde::Deserialize;

/// Candidate `[rerank].model` names. The first is the shipped default.
const CANDIDATES: &[&str] = &[DEFAULT_RERANK_MODEL, "bge-reranker-base"];

const CHUNK_BUDGET_TOKENS: usize = 256;

#[derive(Deserialize)]
struct Dataset {
    memories: Vec<Mem>,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Mem {
    id: String,
    title: String,
    summary: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct Query {
    text: String,
    relevant: BTreeMap<String, u8>,
}

/// Mirrors `retrieval::engine::embed_memory_with` with `metadata_vector = true`:
/// one metadata vector, then the content chunks.
fn doc_texts(m: &Mem) -> Vec<String> {
    let mut head = format!("{}. {}", m.title, m.summary);
    if !m.tags.is_empty() {
        head.push_str(&format!(". tags: {}", m.tags.join(", ")));
    }
    let mut out = vec![head];
    out.extend(chunk_text(&m.content, CHUNK_BUDGET_TOKENS));
    out
}

/// Mirrors `retrieval::engine::rerank_document`.
fn rerank_document(m: &Mem) -> String {
    let mut doc = format!("{}. {} {}", m.title, m.summary, m.content);
    if !m.tags.is_empty() {
        doc.push_str(&format!(" tags: {}", m.tags.join(", ")));
    }
    doc
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x * y) as f64).sum()
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    p1: f64,
    r5: f64,
    mrr: f64,
    ndcg: f64,
}

fn ndcg_at_10(ranked: &[&str], rels: &BTreeMap<String, u8>) -> f64 {
    let dcg: f64 = ranked
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, id)| f64::from(*rels.get(*id).unwrap_or(&0)) / ((i + 2) as f64).log2())
        .sum();
    let mut ideal: Vec<f64> = rels.values().map(|g| f64::from(*g)).collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let idcg: f64 = ideal
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, g)| g / ((i + 2) as f64).log2())
        .sum();
    if idcg > 0.0 {
        dcg / idcg
    } else {
        0.0
    }
}

fn score_all(rankings: &[Vec<&str>], queries: &[Query]) -> Metrics {
    let mut m = Metrics::default();
    for (ranked, q) in rankings.iter().zip(queries) {
        let rels = &q.relevant;
        m.p1 += ranked
            .first()
            .map(|id| f64::from(*rels.get(*id).unwrap_or(&0) >= 1))
            .unwrap_or(0.0);
        let total = rels.values().filter(|g| **g >= 1).count();
        if total > 0 {
            let hits = ranked
                .iter()
                .take(5)
                .filter(|id| *rels.get(**id).unwrap_or(&0) >= 1)
                .count();
            m.r5 += hits as f64 / total.min(5) as f64;
        }
        m.mrr += ranked
            .iter()
            .take(10)
            .position(|id| *rels.get(*id).unwrap_or(&0) == 2)
            .map(|i| 1.0 / (i + 1) as f64)
            .unwrap_or(0.0);
        m.ndcg += ndcg_at_10(ranked, rels);
    }
    let n = queries.len() as f64;
    Metrics {
        p1: m.p1 / n,
        r5: m.r5 / n,
        mrr: m.mrr / n,
        ndcg: m.ndcg / n,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let data_path =
        std::env::var("EMBED_EVAL_DATA").unwrap_or_else(|_| "examples/data/embed_eval.json".into());
    let top_n: usize = std::env::var("RERANK_BENCH_TOP_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let weight: f64 = std::env::var("RERANK_BENCH_WEIGHT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.5);
    let filter: Option<Vec<String>> = std::env::var("RERANK_BENCH_MODELS")
        .ok()
        .map(|s| s.split(',').map(|m| m.trim().to_string()).collect());

    let raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("reading dataset {data_path}"))?;
    let ds: Dataset = serde_json::from_str(&raw).context("parsing dataset")?;

    // --- Stage 1: production bi-encoder ranking -----------------------------
    let provider = OnnxProvider::new().context("loading the default embedding model")?;
    let per_mem: Vec<Vec<String>> = ds.memories.iter().map(doc_texts).collect();

    // Deterministic batch composition (int8 output is batch-sensitive).
    let mut uniq: Vec<String> = per_mem.iter().flatten().cloned().collect();
    uniq.extend(ds.queries.iter().map(|q| q.text.clone()));
    uniq.sort();
    uniq.dedup();
    let mut table = std::collections::HashMap::new();
    for batch in uniq.chunks(32) {
        let refs: Vec<&str> = batch.iter().map(|s| s.as_str()).collect();
        for (text, mut v) in batch.iter().zip(provider.embed_batch(&refs).await?) {
            normalize(&mut v);
            table.insert(text.clone(), v);
        }
    }

    let mut base_rank: Vec<Vec<&str>> = Vec::new();
    let mut base_score: Vec<std::collections::HashMap<&str, f64>> = Vec::new();
    for q in &ds.queries {
        let qv = &table[&q.text];
        let mut scored: Vec<(f64, &str)> = ds
            .memories
            .iter()
            .zip(&per_mem)
            .map(|(m, texts)| {
                let best = texts
                    .iter()
                    .map(|t| cosine(&table[t], qv))
                    .fold(f64::MIN, f64::max);
                (best, m.id.as_str())
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        base_score.push(scored.iter().map(|(s, id)| (*id, *s)).collect());
        base_rank.push(scored.into_iter().map(|(_, id)| id).collect());
    }

    println!(
        "corpus: {} memories, {} queries | bi-encoder: {} | top_n {top_n}, weight {weight}",
        ds.memories.len(),
        ds.queries.len(),
        provider.model_id()
    );
    let baseline = score_all(&base_rank, &ds.queries);
    println!(
        "\n{:<34} {:>8} {:>9} {:>11} {:>7} {:>7} {:>7} {:>7}",
        "reranker", "load_ms", "ms/pair", "ms/query", "P@1", "R@5", "MRR@10", "nDCG@10"
    );
    println!(
        "{:<34} {:>8} {:>9} {:>11} {:>7.3} {:>7.3} {:>7.3} {:>7.3}",
        "(none — bi-encoder only)",
        "-",
        "-",
        "-",
        baseline.p1,
        baseline.r5,
        baseline.mrr,
        baseline.ndcg
    );

    // --- Stage 2: cross-encoder rerank of the top-N -------------------------
    let docs_by_id: std::collections::HashMap<&str, String> = ds
        .memories
        .iter()
        .map(|m| (m.id.as_str(), rerank_document(m)))
        .collect();

    for name in CANDIDATES {
        if filter
            .as_ref()
            .is_some_and(|f| !f.iter().any(|k| k == name))
        {
            continue;
        }
        let t = Instant::now();
        let reranker = match LocalReranker::load(name) {
            Ok(r) => r,
            Err(e) => {
                println!("{name:<34} skipped: {e:#}");
                continue;
            }
        };
        let load_ms = t.elapsed().as_secs_f64() * 1000.0;

        let mut ranked_all: Vec<Vec<&str>> = Vec::new();
        let mut rerank_secs = 0.0;
        let mut pairs = 0usize;
        for (qi, q) in ds.queries.iter().enumerate() {
            let head: Vec<&str> = base_rank[qi].iter().take(top_n).copied().collect();
            let documents: Vec<String> = head.iter().map(|id| docs_by_id[id].clone()).collect();
            let t = Instant::now();
            let scores = reranker.rerank(&q.text, &documents).await?;
            rerank_secs += t.elapsed().as_secs_f64();
            pairs += documents.len();

            let mut blended: Vec<(f64, &str)> = head.iter().map(|id| (f64::MIN, *id)).collect();
            for s in &scores {
                let norm = 1.0 / (1.0 + (-(s.score as f64)).exp());
                blended[s.index].0 = (1.0 - weight) * base_score[qi][head[s.index]] + weight * norm;
            }
            blended.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            let mut out: Vec<&str> = blended.into_iter().map(|(_, id)| id).collect();
            out.extend(base_rank[qi].iter().skip(top_n).copied());
            ranked_all.push(out);
        }

        let m = score_all(&ranked_all, &ds.queries);
        println!(
            "{:<34} {:>8.0} {:>9.1} {:>11.0} {:>7.3} {:>7.3} {:>7.3} {:>7.3}",
            *name,
            load_ms,
            rerank_secs * 1000.0 / pairs as f64,
            rerank_secs * 1000.0 / ds.queries.len() as f64,
            m.p1,
            m.r5,
            m.mrr,
            m.ndcg
        );
    }
    Ok(())
}
