//! Quick recall/quality sanity for int8 vs fp32 all-MiniLM embeddings.
//!
//! Lever B showed int8 (`ONNX_ALL_MINILM_Q`) is 1.4-1.9x faster and far
//! lighter. Speed is moot if retrieval quality regresses, so this checks:
//!
//! 1. Per-text drift: cosine(fp32(t), int8(t)) — how close int8 vectors are
//!    to fp32 for the same text (should be very high, ~>0.97).
//! 2. Ranking agreement: for several (query, relevant, distractor) sets,
//!    does int8 preserve relevant-over-distractor ordering and the fp32
//!    top-1? Retrieval only needs the *ordering* preserved, not identical
//!    vectors.
//!
//! Run: `cargo run --release --example embed_quality`

use anyhow::Result;
use engramdb::embeddings::{EmbeddingProvider, OnnxProvider, ONNX_ALL_MINILM, ONNX_ALL_MINILM_Q};
use engramdb::onnx_ep::Backend;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

/// (query, relevant doc, distractor doc) — semantic, EngramDB-flavored.
const CASES: &[(&str, &str, &str)] = &[
    (
        "how do we run the test suite",
        "Use cargo nextest run instead of cargo test for this repository.",
        "The Core ML execution provider lowers subgraphs onto the Apple Neural Engine.",
    ),
    (
        "why was LanceDB chosen for storage",
        "We picked LanceDB because it embeds in-process with zero external services.",
        "T5-small generates a short abstractive summary used as a memory title.",
    ),
    (
        "what fixed the concurrent crash",
        "The embedding provider is now cached behind an Arc so concurrent tool calls don't poison the mutex.",
        "Logical scope proximity is computed over dotted namespaces like api.auth.",
    ),
    (
        "how are memories ranked",
        "The retrieval engine combines semantic similarity, scope proximity, recency decay and trust weight.",
        "Models download from HuggingFace Hub on first use and cache under the engramdb cache dir.",
    ),
];

#[tokio::main]
async fn main() -> Result<()> {
    let fp32 = OnnxProvider::with_model_on(ONNX_ALL_MINILM, Backend::Cpu)?;
    let int8 = OnnxProvider::with_model_on(ONNX_ALL_MINILM_Q, Backend::Cpu)?;

    let mut drift_sum = 0.0f32;
    let mut drift_min = 1.0f32;
    let mut n = 0;
    let mut fp32_ok = 0;
    let mut int8_ok = 0;
    let mut top1_agree = 0;

    for (query, relevant, distractor) in CASES {
        for text in [query, relevant, distractor] {
            let a = fp32.embed(text).await?;
            let b = int8.embed(text).await?;
            let d = cosine(&a, &b);
            drift_sum += d;
            drift_min = drift_min.min(d);
            n += 1;
        }

        let qf = fp32.embed(query).await?;
        let rf = fp32.embed(relevant).await?;
        let df = fp32.embed(distractor).await?;
        let qi = int8.embed(query).await?;
        let ri = int8.embed(relevant).await?;
        let di = int8.embed(distractor).await?;

        let fp32_ranks_relevant = cosine(&qf, &rf) > cosine(&qf, &df);
        let int8_ranks_relevant = cosine(&qi, &ri) > cosine(&qi, &di);
        fp32_ok += usize::from(fp32_ranks_relevant);
        int8_ok += usize::from(int8_ranks_relevant);
        top1_agree += usize::from(fp32_ranks_relevant == int8_ranks_relevant);

        println!(
            "  q={:?}\n    fp32 rel>dist: {fp32_ranks_relevant}  int8 rel>dist: {int8_ranks_relevant}",
            query
        );
    }

    let cases = CASES.len();
    println!("\nint8 vs fp32 all-MiniLM sanity ({cases} retrieval cases, {n} texts):");
    println!(
        "  per-text cosine(fp32,int8): mean {:.4}  min {:.4}",
        drift_sum / n as f32,
        drift_min
    );
    println!("  relevant>distractor   fp32 {fp32_ok}/{cases}  int8 {int8_ok}/{cases}");
    println!("  ranking agreement fp32==int8: {top1_agree}/{cases}");
    Ok(())
}
