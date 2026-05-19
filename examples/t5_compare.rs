//! A/B comparison of T5-small ONNX model sources for title generation.
//!
//! The historical `ArsenyParamonov/t5-small-onnx` repo is gated (HTTP 401),
//! so we need a replacement. This harness loads two public candidates —
//! [`T5_OPTIMUM`] (fp32, ~376 MB) and [`T5_XENOVA_Q`] (int8 quantized,
//! ~74 MB) — and generates titles for a set of representative EngramDB
//! memory texts so the output quality and latency can be compared directly.
//!
//! Both run on the CPU backend: quantization quality is independent of the
//! execution provider, and CPU is faster than Core ML for these small
//! models (see `benches/benchmarks.rs`, group `onnx_backend`).
//!
//! Run with: `cargo run --release --example t5_compare`

use std::time::Instant;

use anyhow::Result;
use engramdb::onnx_ep::Backend;
use engramdb::title::t5::{T5ModelSpec, T5TitleGenerator, T5_OPTIMUM, T5_XENOVA_Q};
use engramdb::title::TitleGenerator;

/// Representative inputs spanning the memory types EngramDB titles in
/// practice: decisions, bug fixes, conventions, terse notes, and a long
/// technical paragraph.
fn scenarios() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "decision",
            "We chose LanceDB over Qdrant for the vector store because it embeds \
             in-process with zero external services, supports the Arrow columnar \
             format we already use, and gives us transactional file-based storage \
             suitable for project-scoped agent memory.",
        ),
        (
            "bug_fix",
            "Fixed a panic in the MCP server where concurrent tool calls poisoned \
             the embedding provider mutex. The provider is now cached behind an Arc \
             and the lock is never held across an await point.",
        ),
        (
            "convention",
            "All machine learning model downloads must cache to \
             dirs::cache_dir()/engramdb/models so the embedding, reranker, and NLI \
             models are shared across every project and downloaded only once per \
             machine.",
        ),
        (
            "short_note",
            "Use cargo nextest run instead of cargo test for this repository.",
        ),
        (
            "long_technical",
            "The retrieval engine scores each candidate memory by combining a \
             semantic similarity term from the embedding model, a physical scope \
             proximity term based on glob-matched file paths, a logical scope \
             proximity term over dotted namespaces, a recency decay factor, and a \
             provenance trust weight, then optionally reranks the top fifty results \
             with a cross-encoder before applying contradiction filtering.",
        ),
    ]
}

async fn evaluate(label: &str, spec: &T5ModelSpec) -> Result<()> {
    println!("\n=== {label}  ({}) ===", spec.repo);

    let load_start = Instant::now();
    let generator = match T5TitleGenerator::with_spec_on(spec, Backend::Cpu) {
        Ok(generator) => generator,
        Err(error) => {
            println!("  LOAD FAILED: {error:#}");
            return Ok(());
        }
    };
    let load_elapsed = load_start.elapsed();
    println!("  model load: {load_elapsed:.2?}");

    for (scenario_name, input_text) in scenarios() {
        let gen_start = Instant::now();
        let title = generator.generate(input_text).await?;
        let elapsed = gen_start.elapsed();
        println!("  [{scenario_name:<14}] {elapsed:>7.0?}  ->  \"{title}\"");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("T5-small ONNX model A/B comparison for title generation");
    println!("(CPU backend; first run downloads the models)");

    evaluate("optimum/t5-small  [fp32, ~376 MB]", &T5_OPTIMUM).await?;
    evaluate("Xenova/t5-small   [int8,  ~74 MB]", &T5_XENOVA_Q).await?;

    println!("\nDone. Compare title quality per scenario and the latency column.");
    Ok(())
}
