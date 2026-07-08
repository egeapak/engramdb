// Benchmark: ONNX Runtime (`ort`) vs pure-Rust `tract-onnx` on the same
// sentence-embedding pipeline (all-MiniLM-L6-v2, int8 + fp32).
//
// Goal: decide whether tract is a viable inference backend for EngramDB
// (it would fix Intel-Mac, which has no prebuilt ONNX Runtime 1.24).

use std::time::Instant;

use anyhow::{Context, Result};
use tokenizers::Tokenizer;

const HIDDEN: usize = 384;

// Fixed correctness set + latency inputs.
const SENTENCES: &[&str] = &[
    "The quick brown fox jumps over the lazy dog.",
    "EngramDB is a project-scoped persistent memory store for coding agents.",
    "Vector databases enable semantic search over embeddings.",
    "Rust provides memory safety without a garbage collector.",
    "The weather in San Francisco is famously foggy.",
];

struct ModelPaths {
    label: &'static str,
    model: String,
    tokenizer: String,
}

/// A padded batch ready for either engine.
struct Batch {
    batch: usize,
    seq: usize,
    ids: Vec<i64>,   // flat [batch*seq]
    mask: Vec<i64>,  // flat [batch*seq]
    types: Vec<i64>, // flat [batch*seq] (all zeros)
}

fn encode_batch(tok: &Tokenizer, sentences: &[&str]) -> Result<Batch> {
    let encs = tok
        .encode_batch(sentences.to_vec(), true)
        .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
    let batch = encs.len();
    let seq = encs.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);

    let mut ids = vec![0i64; batch * seq];
    let mut mask = vec![0i64; batch * seq];
    let types = vec![0i64; batch * seq];
    for (b, e) in encs.iter().enumerate() {
        let eids = e.get_ids();
        let emask = e.get_attention_mask();
        for t in 0..eids.len() {
            ids[b * seq + t] = eids[t] as i64;
            mask[b * seq + t] = emask[t] as i64;
        }
    }
    Ok(Batch {
        batch,
        seq,
        ids,
        mask,
        types,
    })
}

/// Mean-pool over seq using the attention mask, then L2-normalize each row.
/// `hidden` is flat [batch, seq, HIDDEN]; returns [batch, HIDDEN].
fn pool_and_normalize(hidden: &[f32], batch: usize, seq: usize, mask: &[i64]) -> Vec<Vec<f32>> {
    let mut out = Vec::with_capacity(batch);
    for b in 0..batch {
        let mut acc = vec![0f32; HIDDEN];
        let mut denom = 0f32;
        for t in 0..seq {
            let m = mask[b * seq + t] as f32;
            if m == 0.0 {
                continue;
            }
            denom += m;
            let base = (b * seq + t) * HIDDEN;
            for h in 0..HIDDEN {
                acc[h] += hidden[base + h] * m;
            }
        }
        let denom = denom.max(1e-9);
        for v in acc.iter_mut() {
            *v /= denom;
        }
        // L2 normalize
        let norm: f32 = acc.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for v in acc.iter_mut() {
            *v /= norm;
        }
        out.push(acc);
    }
    out
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    // Rows are already L2-normalized, so cosine == dot.
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ---------------------------------------------------------------------------
// ort engine
// ---------------------------------------------------------------------------

struct OrtEngine {
    session: ort::session::Session,
    out_name: String,
}

impl OrtEngine {
    fn load(model_path: &str) -> Result<Self> {
        use ort::session::Session;
        let session = Session::builder()?.commit_from_file(model_path)?;
        // Determine the hidden-state output name (usually last_hidden_state).
        let out_name = session
            .outputs()
            .iter()
            .find(|o| o.name().contains("last_hidden_state") || o.name().contains("hidden"))
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| session.outputs()[0].name().to_string());
        Ok(Self { session, out_name })
    }

    fn infer(&mut self, b: &Batch) -> Result<Vec<Vec<f32>>> {
        use ort::value::Tensor;
        let ids = Tensor::from_array((vec![b.batch as i64, b.seq as i64], b.ids.clone()))?;
        let mask = Tensor::from_array((vec![b.batch as i64, b.seq as i64], b.mask.clone()))?;
        let types = Tensor::from_array((vec![b.batch as i64, b.seq as i64], b.types.clone()))?;
        let outputs = self.session.run(ort::inputs![
            "input_ids" => ids,
            "attention_mask" => mask,
            "token_type_ids" => types,
        ])?;
        let (shape, data) = outputs[self.out_name.as_str()].try_extract_tensor::<f32>()?;
        // shape is [batch, seq, HIDDEN]
        let seq = shape[1] as usize;
        Ok(pool_and_normalize(data, b.batch, seq, &b.mask))
    }
}

// ---------------------------------------------------------------------------
// tract engine
// ---------------------------------------------------------------------------

use tract_onnx::prelude::*;

type TractModel = std::sync::Arc<RunnableModel<TypedFact, Box<dyn TypedOp>>>;

struct TractEngine {
    model: TractModel,
    // For each graph input outlet (in run() order) which logical role it is.
    input_roles: Vec<InputRole>,
    optimized: bool,
}

#[derive(Clone, Copy, Debug)]
enum InputRole {
    Ids,
    Mask,
    Types,
}

fn classify(name: &str) -> InputRole {
    let n = name.to_lowercase();
    if n.contains("token_type") || n.contains("type_ids") {
        InputRole::Types
    } else if n.contains("attention") || n.contains("mask") {
        InputRole::Mask
    } else {
        InputRole::Ids
    }
}

impl TractEngine {
    /// Load with concrete facts for a specific (batch, seq). tract needs
    /// concrete shapes, so we build one runnable per batch size.
    fn load(model_path: &str, batch: usize, seq: usize) -> Result<Self> {
        let mut infer = tract_onnx::onnx()
            .model_for_path(model_path)
            .with_context(|| format!("tract model_for_path {model_path}"))?;

        // Discover input names/order from the inference graph.
        let input_names: Vec<String> = infer
            .input_outlets()?
            .iter()
            .map(|o| infer.node(o.node).name.clone())
            .collect();
        let input_roles: Vec<InputRole> = input_names.iter().map(|n| classify(n)).collect();
        eprintln!("      tract inputs (order): {input_names:?} -> roles {input_roles:?}");

        // Set concrete i64 [batch, seq] facts for every input, in graph order.
        for i in 0..input_names.len() {
            infer = infer.with_input_fact(
                i,
                InferenceFact::dt_shape(i64::datum_type(), tvec!(batch, seq)),
            )?;
        }

        // Try optimized; fall back to typed-unoptimized if optimization fails
        // (the key risk for the int8/QDQ quantized model).
        let (model, optimized) = match infer.clone().into_optimized() {
            Ok(opt) => (opt.into_runnable()?, true),
            Err(e) => {
                eprintln!("      into_optimized() FAILED: {e}");
                eprintln!("      falling back to typed (unoptimized)...");
                let typed = match infer.into_typed() {
                    Ok(t) => t,
                    Err(te) => {
                        eprintln!("      into_typed() ALSO FAILED: {te:?}");
                        anyhow::bail!("both into_optimized and into_typed failed: opt=[{e}] typed=[{te}]");
                    }
                };
                (typed.into_runnable()?, false)
            }
        };

        Ok(Self {
            model,
            input_roles,
            optimized,
        })
    }

    fn infer(&self, b: &Batch) -> Result<Vec<Vec<f32>>> {
        let make = |role: InputRole| -> TValue {
            let flat = match role {
                InputRole::Ids => b.ids.clone(),
                InputRole::Mask => b.mask.clone(),
                InputRole::Types => b.types.clone(),
            };
            let t: Tensor = tract_ndarray::Array2::<i64>::from_shape_vec((b.batch, b.seq), flat)
                .unwrap()
                .into();
            t.into()
        };
        let inputs: TVec<TValue> = self.input_roles.iter().map(|r| make(*r)).collect();
        let result = self.model.run(inputs)?;
        let view = result[0].to_plain_array_view::<f32>()?;
        let data: Vec<f32> = view.iter().copied().collect();
        Ok(pool_and_normalize(&data, b.batch, b.seq, &b.mask))
    }
}

// ---------------------------------------------------------------------------
// Benchmark harness
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Timing {
    single_p50: f64,
    single_mean: f64,
    batch8_p50: f64,
    batch8_mean: f64,
}

fn stats(mut xs: Vec<f64>) -> (f64, f64) {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = xs[xs.len() / 2];
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    (p50, mean)
}

fn bench_single<F>(mut run: F, iters: usize) -> (f64, f64)
where
    F: FnMut() -> Result<()>,
{
    for _ in 0..3 {
        let _ = run();
    }
    let mut times = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        run().unwrap();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    stats(times)
}

#[derive(Clone)]
struct Row {
    label: String,
    ran: bool,
    optimized: Option<bool>,
    timing: Timing,
    cosine_vs_ort: Option<f32>,
    note: String,
}

fn main() -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let cache = format!("{home}/.cache/engramdb/models");

    let models = [
        ModelPaths {
            label: "int8",
            model: format!(
                "{cache}/models--Xenova--all-MiniLM-L6-v2/snapshots/main/onnx/model_quantized.onnx"
            ),
            tokenizer: format!(
                "{cache}/models--Xenova--all-MiniLM-L6-v2/snapshots/main/tokenizer.json"
            ),
        },
        ModelPaths {
            label: "fp32",
            model: format!(
                "{cache}/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/main/model.onnx"
            ),
            tokenizer: format!(
                "{cache}/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/main/tokenizer.json"
            ),
        },
    ];

    let mut rows: Vec<Row> = Vec::new();

    for mp in &models {
        println!("\n=== Model: {} ({}) ===", mp.label, mp.model);
        let tok = match Tokenizer::from_file(&mp.tokenizer) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("  tokenizer load failed: {e}");
                continue;
            }
        };

        // Per-sentence single batches (for correctness + single-encode latency).
        let singles: Vec<Batch> = SENTENCES
            .iter()
            .map(|s| encode_batch(&tok, &[s]).unwrap())
            .collect();
        let batch8_src: Vec<&str> = SENTENCES
            .iter()
            .cycle()
            .take(8)
            .copied()
            .collect();
        let batch8 = encode_batch(&tok, &batch8_src)?;

        // ---- ort ----
        let ort_embeds: Option<Vec<Vec<f32>>> = match OrtEngine::load(&mp.model) {
            Ok(mut eng) => {
                println!("  [ort] output name: {}", eng.out_name);
                // correctness embeddings (one per sentence)
                let mut embeds = Vec::new();
                let mut ok = true;
                for s in &singles {
                    match eng.infer(s) {
                        Ok(mut e) => embeds.push(e.remove(0)),
                        Err(err) => {
                            eprintln!("  [ort] infer failed: {err}");
                            ok = false;
                            break;
                        }
                    }
                }
                let mut timing = Timing::default();
                let s0 = &singles[0];
                let (p50, mean) = bench_single(|| eng.infer(s0).map(|_| ()), 50);
                timing.single_p50 = p50;
                timing.single_mean = mean;
                let (p50, mean) = bench_single(|| eng.infer(&batch8).map(|_| ()), 20);
                timing.batch8_p50 = p50;
                timing.batch8_mean = mean;
                rows.push(Row {
                    label: format!("ort {}", mp.label),
                    ran: true,
                    optimized: None,
                    timing,
                    cosine_vs_ort: Some(1.0),
                    note: String::new(),
                });
                if ok {
                    Some(embeds)
                } else {
                    None
                }
            }
            Err(e) => {
                eprintln!("  [ort] load failed: {e}");
                rows.push(Row {
                    label: format!("ort {}", mp.label),
                    ran: false,
                    optimized: None,
                    timing: Timing::default(),
                    cosine_vs_ort: None,
                    note: format!("load failed: {e}"),
                });
                None
            }
        };

        // ---- tract ----
        // tract needs concrete shapes -> one runnable per (batch, seq).
        // Build a runnable for the single-encode seq and one for batch8.
        // For correctness we reuse per-sentence runnables (seq varies), so
        // build a runnable per single batch too. To keep it cheap, load once
        // per distinct seq length.
        let mut tract_note = String::new();
        let mut tract_optimized: Option<bool> = None;
        let mut tract_embeds: Option<Vec<Vec<f32>>> = Some(Vec::new());
        let mut tract_ran = true;

        // correctness: per-sentence
        for (i, s) in singles.iter().enumerate() {
            match TractEngine::load(&mp.model, s.batch, s.seq) {
                Ok(eng) => {
                    tract_optimized = Some(eng.optimized);
                    match eng.infer(s) {
                        Ok(mut e) => {
                            if let Some(v) = tract_embeds.as_mut() {
                                v.push(e.remove(0));
                            }
                        }
                        Err(err) => {
                            eprintln!("  [tract] infer failed (sentence {i}): {err}");
                            tract_note = format!("infer failed: {err}");
                            tract_ran = false;
                            tract_embeds = None;
                            break;
                        }
                    }
                }
                Err(err) => {
                    eprintln!("  [tract] load failed (sentence {i}): {err}");
                    tract_note = format!("load failed: {err}");
                    tract_ran = false;
                    tract_embeds = None;
                    break;
                }
            }
        }

        let mut tract_timing = Timing::default();
        if tract_ran {
            // single-encode latency (reuse sentence 0)
            let s0 = &singles[0];
            if let Ok(eng) = TractEngine::load(&mp.model, s0.batch, s0.seq) {
                let (p50, mean) = bench_single(|| eng.infer(s0).map(|_| ()), 50);
                tract_timing.single_p50 = p50;
                tract_timing.single_mean = mean;
            }
            if let Ok(eng) = TractEngine::load(&mp.model, batch8.batch, batch8.seq) {
                let (p50, mean) = bench_single(|| eng.infer(&batch8).map(|_| ()), 20);
                tract_timing.batch8_p50 = p50;
                tract_timing.batch8_mean = mean;
            }
        }

        // correctness: cosine tract-vs-ort (min over sentences)
        let cosine_vs_ort = match (&ort_embeds, &tract_embeds) {
            (Some(o), Some(t)) if o.len() == t.len() && !t.is_empty() => {
                let min = o
                    .iter()
                    .zip(t)
                    .map(|(a, b)| cosine(a, b))
                    .fold(f32::INFINITY, f32::min);
                Some(min)
            }
            _ => None,
        };

        if tract_optimized == Some(false) && tract_note.is_empty() {
            tract_note = "ran UNOPTIMIZED (into_optimized failed)".to_string();
        }

        rows.push(Row {
            label: format!("tract {}", mp.label),
            ran: tract_ran,
            optimized: tract_optimized,
            timing: tract_timing,
            cosine_vs_ort,
            note: tract_note,
        });
    }

    // ---- summary table ----
    println!("\n\n================ SUMMARY ================");
    println!(
        "{:<14} {:>12} {:>12} {:>14} {:>10}  {}",
        "engine/model", "single p50", "batch8 p50", "cosine-vs-ort", "opt?", "note"
    );
    println!("{}", "-".repeat(90));
    for r in &rows {
        let single = if r.ran {
            format!("{:.2} ms", r.timing.single_p50)
        } else {
            "-".into()
        };
        let batch8 = if r.ran {
            format!("{:.2} ms", r.timing.batch8_p50)
        } else {
            "-".into()
        };
        let cos = match r.cosine_vs_ort {
            Some(c) => format!("{c:.5}"),
            None => "-".into(),
        };
        let opt = match r.optimized {
            Some(true) => "yes",
            Some(false) => "NO",
            None => "n/a",
        };
        println!(
            "{:<14} {:>12} {:>12} {:>14} {:>10}  {}",
            r.label, single, batch8, cos, opt, r.note
        );
    }

    // slowdown factors
    println!("\n---- tract/ort slowdown (p50) ----");
    for lbl in ["int8", "fp32"] {
        let ort = rows
            .iter()
            .find(|r| r.label == format!("ort {lbl}") && r.ran);
        let tract = rows
            .iter()
            .find(|r| r.label == format!("tract {lbl}") && r.ran);
        if let (Some(o), Some(t)) = (ort, tract) {
            println!(
                "  {lbl}: single {:.2}x   batch8 {:.2}x",
                t.timing.single_p50 / o.timing.single_p50,
                t.timing.batch8_p50 / o.timing.batch8_p50
            );
        } else {
            println!("  {lbl}: (incomplete — one engine did not run)");
        }
    }

    Ok(())
}
