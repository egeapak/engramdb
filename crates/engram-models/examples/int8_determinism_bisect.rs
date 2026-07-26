//! Bisect the int8 embedding reproducibility failure across the three layers
//! that could be responsible.
//!
//! `examples/embed_determinism_probe.rs` (root crate) established the symptom:
//! under CPU load, the int8 MiniLM models return materially different vectors
//! for the same text — up to 45 distinct vectors across 60 calls, minimum
//! pairwise cosine below 0 — while fp32 is bit-exact. Thread count does not
//! change it. This narrows *where* it happens:
//!
//! - **L1 `OnnxProvider`** — EngramDB's wrapper: `tokio::spawn_blocking` plus
//!   `Arc<Mutex<TextEmbedding>>`.
//! - **L2 `fastembed::TextEmbedding`** — same thread, no tokio, no mutex.
//!   Includes tokenization, the ndarray build, `session.run`, and pooling.
//! - **L3 raw `ort::Session`** — the *same model file*, driven with an input
//!   tensor that is tokenized **once** and reused verbatim for every run, so
//!   the only thing that can vary is ONNX Runtime itself.
//!
//! Whichever is the first layer to show variance owns the bug. L3 also reports
//! the raw `last_hidden_state`, so a stable L3 with an unstable L2 would point
//! at pooling rather than inference.
//!
//! Run: `cargo run --release -p engram-models --example int8_determinism_bisect`
//! Env: `BISECT_ITERS` (default 40), `BISECT_LOAD_THREADS` (default 8).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use engram_models::embeddings::{
    EmbeddingProvider, OnnxProvider, ONNX_ALL_MINILM, ONNX_ALL_MINILM_L12_Q, ONNX_ALL_MINILM_Q,
};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use ndarray::Array2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Value;
use tokenizers::Tokenizer;

/// A realistic memory-sized chunk: the probe showed severity scales with
/// sequence length, so the bisect uses the worst case.
const TEXT: &str = "We chose LanceDB over Qdrant for the vector store because it embeds \
     in-process with zero external services, supports the Arrow columnar format we already \
     use for the metadata table, and lets a single table hold both the filterable metadata \
     and the embedding vectors. The alternative would have meant running a separate service \
     per project, which is unacceptable for a tool that has to start instantly inside a \
     coding agent session and leave no daemon behind when it exits.";

struct Case {
    label: &'static str,
    fastembed_model: EmbeddingModel,
    repo: &'static str,
    file: &'static str,
}

const CASES: &[Case] = &[
    Case {
        label: "L12-q (int8, default)",
        fastembed_model: EmbeddingModel::AllMiniLML12V2Q,
        repo: "Xenova/all-MiniLM-L12-v2",
        file: "onnx/model_quantized.onnx",
    },
    Case {
        label: "L6-q  (int8)",
        fastembed_model: EmbeddingModel::AllMiniLML6V2Q,
        repo: "Xenova/all-MiniLM-L6-v2",
        file: "onnx/model_quantized.onnx",
    },
    Case {
        label: "L6    (fp32 control)",
        fastembed_model: EmbeddingModel::AllMiniLML6V2,
        repo: "Qdrant/all-MiniLM-L6-v2-onnx",
        file: "model.onnx",
    },
];

fn distinct(vectors: &[Vec<f32>]) -> usize {
    let mut seen: Vec<&Vec<f32>> = Vec::new();
    for v in vectors {
        if !seen.iter().any(|s| s.len() == v.len() && *s == v) {
            seen.push(v);
        }
    }
    seen.len()
}

fn max_spread(vectors: &[Vec<f32>]) -> f32 {
    let mut worst = 0.0f32;
    for i in 0..vectors[0].len() {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for v in vectors {
            lo = lo.min(v[i]);
            hi = hi.max(v[i]);
        }
        worst = worst.max(hi - lo);
    }
    worst
}

fn report(layer: &str, label: &str, vectors: &[Vec<f32>]) {
    println!(
        "  {layer:<34} {label:<22} distinct {:>3}/{:<3}  max spread {:.6}",
        distinct(vectors),
        vectors.len(),
        max_spread(vectors),
    );
}

/// L1: through EngramDB's provider (tokio blocking pool + mutex).
async fn l1(case: &Case, iters: usize) -> Result<Option<Vec<Vec<f32>>>> {
    let spec = match case.label.chars().next() {
        _ if case.fastembed_model == EmbeddingModel::AllMiniLML12V2Q => ONNX_ALL_MINILM_L12_Q,
        _ if case.fastembed_model == EmbeddingModel::AllMiniLML6V2Q => ONNX_ALL_MINILM_Q,
        _ => ONNX_ALL_MINILM,
    };
    let Ok(provider) = OnnxProvider::with_model(spec) else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(iters);
    for _ in 0..iters {
        out.push(provider.embed(TEXT).await?);
    }
    Ok(Some(out))
}

/// L2: fastembed directly — same thread, no tokio, no mutex.
fn l2(case: &Case, iters: usize) -> Result<Option<Vec<Vec<f32>>>> {
    let cache_dir = engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{e}"))?;
    let options = InitOptions::new(case.fastembed_model.clone())
        .with_cache_dir(cache_dir)
        .with_max_length(256);
    let Ok(mut model) = TextEmbedding::try_new(options) else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(iters);
    for _ in 0..iters {
        out.push(model.embed(vec![TEXT.to_string()], None)?.remove(0));
    }
    Ok(Some(out))
}

/// L3: raw ONNX Runtime, one tokenization reused for every run. If this
/// varies, nothing above ORT can be blamed.
fn l3(case: &Case, iters: usize, opts: L3Opts) -> Result<Option<Vec<Vec<f32>>>> {
    let cache_dir = engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{e}"))?;
    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(cache_dir)
        .build()
        .context("init hf api")?;
    let repo = api.model(case.repo.to_string());
    let (Ok(model_path), Ok(tok_path)) = (repo.get(case.file), repo.get("tokenizer.json")) else {
        return Ok(None);
    };

    let tokenizer =
        Tokenizer::from_file(&tok_path).map_err(|e| anyhow::anyhow!("load tokenizer: {e}"))?;
    let enc = tokenizer
        .encode(TEXT, true)
        .map_err(|e| anyhow::anyhow!("encode: {e}"))?;
    let len = enc.get_ids().len().min(256);
    let ids: Vec<i64> = enc.get_ids()[..len].iter().map(|&x| x as i64).collect();
    let mask: Vec<i64> = enc.get_attention_mask()[..len]
        .iter()
        .map(|&x| x as i64)
        .collect();
    let types = vec![0i64; len];

    run_l3(&model_path, &ids, &mask, &types, len, iters, opts)
}

/// Session knobs to sweep once L3 is confirmed as the guilty layer. Each
/// isolates one candidate mechanism inside ONNX Runtime.
#[derive(Clone, Copy)]
struct L3Opts {
    label: &'static str,
    opt_level: GraphOptimizationLevel,
    intra: usize,
    /// ORT's memory-pattern planner pre-plans and reuses one big buffer.
    memory_pattern: bool,
    /// ORT's own "make results reproducible" switch.
    deterministic: bool,
}

const L3_SWEEP: &[L3Opts] = &[
    L3Opts {
        label: "baseline (L3 opt, 4 thr)",
        opt_level: GraphOptimizationLevel::Level3,
        intra: 4,
        memory_pattern: true,
        deterministic: false,
    },
    L3Opts {
        label: "intra=1",
        opt_level: GraphOptimizationLevel::Level3,
        intra: 1,
        memory_pattern: true,
        deterministic: false,
    },
    L3Opts {
        label: "deterministic_compute",
        opt_level: GraphOptimizationLevel::Level3,
        intra: 4,
        memory_pattern: true,
        deterministic: true,
    },
    L3Opts {
        label: "memory_pattern=false",
        opt_level: GraphOptimizationLevel::Level3,
        intra: 4,
        memory_pattern: false,
        deterministic: false,
    },
    L3Opts {
        label: "no graph optimization",
        opt_level: GraphOptimizationLevel::Disable,
        intra: 4,
        memory_pattern: true,
        deterministic: false,
    },
];

#[allow(clippy::too_many_arguments)]
fn run_l3(
    model_path: &std::path::Path,
    ids: &[i64],
    mask: &[i64],
    types: &[i64],
    len: usize,
    iters: usize,
    opts: L3Opts,
) -> Result<Option<Vec<Vec<f32>>>> {
    // `SessionBuilder`'s error type is neither Send nor Sync, so it can't ride
    // `?` into anyhow — same dance as `nli::onnx`.
    let to_err = |e: ort::Error<ort::session::builder::SessionBuilder>| anyhow::anyhow!("{e}");
    let mut builder = Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_optimization_level(opts.opt_level)
        .map_err(to_err)?
        .with_intra_threads(opts.intra)
        .map_err(to_err)?
        .with_memory_pattern(opts.memory_pattern)
        .map_err(to_err)?;
    if opts.deterministic {
        builder = builder.with_deterministic_compute(true).map_err(to_err)?;
    }
    let mut session = builder.commit_from_file(model_path)?;
    let needs_types = session
        .inputs()
        .iter()
        .any(|i| i.name() == "token_type_ids");

    let mut out = Vec::with_capacity(iters);
    for _ in 0..iters {
        // Fresh Values each run (ort takes ownership), but from the SAME
        // tokenized data — byte-identical input every iteration.
        let ids_a = Array2::from_shape_vec((1, len), ids.to_vec())?;
        let mask_a = Array2::from_shape_vec((1, len), mask.to_vec())?;
        let mut inputs = ort::inputs![
            "input_ids" => Value::from_array(ids_a)?,
            "attention_mask" => Value::from_array(mask_a)?,
        ];
        if needs_types {
            let t = Array2::from_shape_vec((1, len), types.to_vec())?;
            inputs.push(("token_type_ids".into(), Value::from_array(t)?.into()));
        }
        let outputs = session.run(inputs)?;
        let (_, first) = outputs.iter().next().context("no output")?;
        let (_, data) = first.try_extract_tensor::<f32>()?;
        // Mean-pool over the sequence so the summary is comparable in size to
        // an embedding; corruption anywhere in the hidden states shows up here.
        let hidden = data.len() / len;
        let mut pooled = vec![0f32; hidden];
        for t in 0..len {
            for h in 0..hidden {
                pooled[h] += data[t * hidden + h];
            }
        }
        pooled.iter_mut().for_each(|x| *x /= len as f32);
        out.push(pooled);
    }
    Ok(Some(out))
}

#[tokio::main]
async fn main() -> Result<()> {
    let iters: usize = std::env::var("BISECT_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let load: usize = std::env::var("BISECT_LOAD_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for _ in 0..load {
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(Ordering::Relaxed) {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                std::hint::black_box(x);
            }
        }));
    }

    println!(
        "{iters} repeats of one {}-char text, {load} background load threads\n\
         (distinct = how many different results; 1 = reproducible)",
        TEXT.len()
    );
    for case in CASES {
        println!("-- {} --", case.label);
        if let Some(v) = l1(case, iters).await? {
            report("L1 OnnxProvider (tokio)", case.label, &v);
        }
        if let Some(v) = l2(case, iters)? {
            report("L2 fastembed (1 thread)", case.label, &v);
        }
        for opts in L3_SWEEP {
            if let Some(v) = l3(case, iters, *opts)? {
                report(&format!("L3 raw ort: {}", opts.label), case.label, &v);
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}
