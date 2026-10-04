//! Stage 3 of the burn-onnx import test: run each generated model on the
//! Flex backend and compare it with ONNX Runtime on the same inputs.
//!
//! For every model it reports
//! - agreement with ONNX Runtime fp32 (same weights, same precision), and the
//!   task-level outcome (ranking, label, generated title) against both ORT
//!   fp32 and the quantized file EngramDB ships;
//! - load time and warm per-call latency (p50) for Burn, ORT fp32 and ORT
//!   quantized.
//!
//! Inputs are real: the queries and memories of
//! `examples/data/embed_eval.json`. Output: `../results/verify.md`.

// Generated code is no_std-style and names `alloc::` directly.
extern crate alloc;

mod models;

use anyhow::{anyhow, Context, Result};
use burn::prelude::*;
use ort::session::{builder::GraphOptimizationLevel, Session};
use serde::Deserialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

type B = burn::backend::Flex;
type Dev = burn::tensor::Device<B>;
/// NdArray (64-bit integers): the DeBERTa graph mixes its own I64 values with
/// I32 ones on Flex, which only implements `Backend` for `<f32, i32>`.
type B64 = burn::backend::NdArray;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Dataset {
    memories: Vec<Memory>,
    queries: Vec<Query>,
}
#[derive(Deserialize)]
struct Memory {
    title: String,
    summary: String,
    content: String,
}
#[derive(Deserialize)]
struct Query {
    text: String,
}

/// Flat f32 output with its shape.
struct Out {
    shape: Vec<usize>,
    data: Vec<f32>,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn p50(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn iters() -> usize {
    std::env::var("ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
}

/// Warm p50 of `f`, after one untimed warm-up call.
fn time<F: FnMut() -> Result<()>>(mut f: F) -> Result<f64> {
    f()?;
    let mut s = Vec::new();
    for _ in 0..iters() {
        let t = Instant::now();
        f()?;
        s.push(ms(t));
    }
    Ok(p50(s))
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "output length mismatch");
    a.iter()
        .zip(b)
        .map(|(x, y)| ((x - y) as f64).abs())
        .fold(0.0, f64::max)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    dot / (na * nb).max(1e-12)
}

fn argmax(v: &[f32]) -> usize {
    v.iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(i, _)| i)
        .unwrap()
}

fn ort_session(path: &Path) -> Result<Session> {
    Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow!("{e}"))?
        .commit_from_file(path)
        .with_context(|| format!("load {}", path.display()))
}

/// Integer `[batch, seq]` inputs and float inputs of any shape, by name.
/// Inputs the session does not declare are skipped (e.g. `token_type_ids`).
fn ort_run(
    s: &mut Session,
    ints: &[(&str, [usize; 2], &[i64])],
    floats: &[(&str, &[usize], &[f32])],
) -> Result<Vec<Out>> {
    let declared: Vec<String> = s.inputs().iter().map(|i| i.name().to_string()).collect();
    let mut inputs: Vec<(String, ort::session::SessionInputValue)> = Vec::new();
    for (name, shape, data) in ints {
        if declared.iter().any(|d| d == name) {
            let t = ort::value::Tensor::from_array((*shape, data.to_vec()))?;
            inputs.push((name.to_string(), t.into()));
        }
    }
    for (name, shape, data) in floats {
        if declared.iter().any(|d| d == name) {
            let t = ort::value::Tensor::from_array((shape.to_vec(), data.to_vec()))?;
            inputs.push((name.to_string(), t.into()));
        }
    }
    let outputs = s.run(inputs)?;
    let mut res = Vec::new();
    for (_, v) in outputs.iter() {
        let (shape, data) = v.try_extract_tensor::<f32>()?;
        res.push(Out {
            shape: shape.iter().map(|&d| d as usize).collect(),
            data: data.to_vec(),
        });
    }
    Ok(res)
}

/// Integer input in the backend's own int type (`i32` on Flex). Burn keeps
/// the dtype of the data it is given, and generated code that mixes an `I64`
/// input with its own `I32` constants panics (seen in the DeBERTa graph).
fn int2(v: &[i64], shape: [usize; 2], dev: &Dev) -> Tensor<B, 2, Int> {
    let v: Vec<i32> = v.iter().map(|&x| x as i32).collect();
    Tensor::from_data(TensorData::new(v, shape), dev)
}

fn int64(v: &[i64], shape: [usize; 2]) -> Tensor<B64, 2, Int> {
    Tensor::from_data(TensorData::new(v.to_vec(), shape), &Default::default())
}

fn data64<const D: usize>(t: Tensor<B64, D>) -> Out {
    let shape = t.dims().to_vec();
    Out {
        shape,
        data: t.into_data().into_vec::<f32>().expect("f32 output"),
    }
}

fn data<const D: usize>(t: Tensor<B, D>) -> Out {
    let shape = t.dims().to_vec();
    Out {
        shape,
        data: t.into_data().into_vec::<f32>().expect("f32 output"),
    }
}

/// Encoded batch, padded to the longest row.
struct Enc {
    shape: [usize; 2],
    ids: Vec<i64>,
    mask: Vec<i64>,
    types: Vec<i64>,
}

fn tokenizer(path: &Path, max_len: usize) -> Result<Tokenizer> {
    let mut t = Tokenizer::from_file(path).map_err(|e| anyhow!("{e}"))?;
    t.with_truncation(Some(TruncationParams {
        max_length: max_len,
        ..Default::default()
    }))
    .map_err(|e| anyhow!("{e}"))?;
    t.with_padding(Some(PaddingParams::default()));
    Ok(t)
}

fn enc_from(e: Vec<tokenizers::Encoding>) -> Enc {
    let (b, s) = (e.len(), e[0].len());
    let mut r = Enc {
        shape: [b, s],
        ids: vec![],
        mask: vec![],
        types: vec![],
    };
    for x in &e {
        r.ids.extend(x.get_ids().iter().map(|&v| v as i64));
        r.mask
            .extend(x.get_attention_mask().iter().map(|&v| v as i64));
        r.types.extend(x.get_type_ids().iter().map(|&v| v as i64));
    }
    r
}

fn encode(t: &Tokenizer, texts: &[&str]) -> Result<Enc> {
    Ok(enc_from(
        t.encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("{e}"))?,
    ))
}

fn encode_pairs(t: &Tokenizer, pairs: &[(&str, &str)]) -> Result<Enc> {
    let inputs: Vec<tokenizers::EncodeInput> = pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()).into())
        .collect();
    Ok(enc_from(
        t.encode_batch(inputs, true).map_err(|e| anyhow!("{e}"))?,
    ))
}

struct Ctx {
    dev: Dev,
    gen: PathBuf,
    onnx: PathBuf,
    ds: Dataset,
    report: String,
    speed_rows: Vec<String>,
}

impl Ctx {
    fn docs(&self) -> Vec<String> {
        self.ds
            .memories
            .iter()
            .map(|m| format!("{}. {} {}", m.title, m.summary, m.content))
            .collect()
    }
    fn line(&mut self, s: String) {
        eprintln!("{s}");
        self.report.push_str(&s);
        self.report.push('\n');
    }
    /// Load the generated Burn model's weights (`.bpk` next to the source).
    fn bpk(&self, dir: &str, stem: &str) -> PathBuf {
        self.gen.join(dir).join(format!("{stem}.bpk"))
    }
}

fn timed<T>(f: impl FnOnce() -> Result<T>) -> Result<(T, f64)> {
    let t = Instant::now();
    let v = f()?;
    Ok((v, ms(t)))
}

// ---------------------------------------------------------------------------
// MiniLM embeddings
// ---------------------------------------------------------------------------

fn minilm(c: &mut Ctx) -> Result<()> {
    let tok = tokenizer(&c.onnx.join("../xenova/tokenizer.json"), 256)?;
    let (burn, burn_load) = timed(|| {
        Ok(models::minilm::Model::<B>::from_file(
            c.bpk("minilm-fp32", "minilm-model"),
            &c.dev,
        ))
    })?;
    let (mut f32s, f32_load) = timed(|| ort_session(&c.onnx.join("minilm-model.onnx")))?;
    let (mut q8, q8_load) = timed(|| ort_session(&c.onnx.join("minilm-model_uint8.onnx")))?;

    let docs = c.docs();
    let mut texts: Vec<&str> = docs.iter().map(String::as_str).take(10).collect();
    texts.extend(c.ds.queries.iter().take(10).map(|q| q.text.as_str()));
    let (mut worst_diff, mut min_cos) = (0f64, 1f64);
    for t in &texts {
        let e = encode(&tok, &[t])?;
        let b = data(burn.forward(
            int2(&e.ids, e.shape, &c.dev),
            int2(&e.mask, e.shape, &c.dev),
            int2(&e.types, e.shape, &c.dev),
        ));
        let o = &ort_run(
            &mut f32s,
            &[
                ("input_ids", e.shape, &e.ids),
                ("attention_mask", e.shape, &e.mask),
                ("token_type_ids", e.shape, &e.types),
            ],
            &[],
        )?[0];
        worst_diff = worst_diff.max(max_abs_diff(&b.data, &o.data));
        min_cos = min_cos.min(cosine(&b.data, &o.data));
    }
    c.line(format!(
        "| MiniLM-L12 embeddings | {} texts | max abs diff {worst_diff:.2e}, min cosine {min_cos:.6} (last_hidden_state) |",
        texts.len()
    ));

    let q = encode(&tok, &[c.ds.queries[0].text.as_str()])?;
    let d = encode(&tok, &[docs[1].as_str()])?;
    let mut lat = Vec::new();
    for e in [&q, &d] {
        let bt = time(|| {
            burn.forward(
                int2(&e.ids, e.shape, &c.dev),
                int2(&e.mask, e.shape, &c.dev),
                int2(&e.types, e.shape, &c.dev),
            )
            .into_data();
            Ok(())
        })?;
        let ins = [
            ("input_ids", e.shape, &e.ids[..]),
            ("attention_mask", e.shape, &e.mask[..]),
            ("token_type_ids", e.shape, &e.types[..]),
        ];
        let ft = time(|| ort_run(&mut f32s, &ins, &[]).map(|_| ()))?;
        let qt = time(|| ort_run(&mut q8, &ins, &[]).map(|_| ()))?;
        lat.push((e.shape[1], bt, ft, qt));
    }
    for (len, bt, ft, qt) in lat {
        c.speed(
            &format!("MiniLM, {len} tokens"),
            burn_load,
            f32_load,
            q8_load,
            bt,
            ft,
            qt,
        );
    }
    Ok(())
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    fn speed(&mut self, what: &str, bl: f64, fl: f64, ql: f64, bt: f64, ft: f64, qt: f64) {
        let s = format!(
            "| {what} | {bl:.0} / {fl:.0} / {ql:.0} | {bt:.1} | {ft:.1} | {qt:.1} | {:.1}× | {:.1}× |",
            bt / ft,
            bt / qt
        );
        self.speed_rows.push(s);
    }
}

// ---------------------------------------------------------------------------
// Cross-encoders: reranker (1 logit) and NLI (3 logits)
// ---------------------------------------------------------------------------

/// Query/document pairs: each query against every document, as the engine
/// reranks a query's top candidates.
fn pairs<'a>(queries: &'a [String], docs: &'a [String]) -> Vec<(&'a str, &'a str)> {
    let mut v = Vec::new();
    for q in queries {
        for d in docs {
            v.push((q.as_str(), d.as_str()));
        }
    }
    v
}

fn reranker(c: &mut Ctx) -> Result<()> {
    let dir = c.onnx.join("reranker");
    let tok = tokenizer(&dir.join("tokenizer.json"), 512)?;
    let (burn, burn_load) = timed(|| {
        Ok(models::reranker::Model::<B>::from_file(
            c.bpk("reranker-fp32", "model"),
            &c.dev,
        ))
    })?;
    let (mut f32s, f32_load) = timed(|| ort_session(&dir.join("model.onnx")))?;
    let (mut q8, q8_load) = timed(|| ort_session(&dir.join("model_uint8.onnx")))?;
    let (nq, nd) = (8, 10);
    let docs: Vec<String> = c.docs().into_iter().take(nd).collect();
    let queries: Vec<String> =
        c.ds.queries
            .iter()
            .take(nq)
            .map(|q| q.text.clone())
            .collect();
    let ps = pairs(&queries, &docs);
    let (mut worst, mut top1_f32, mut top1_q8) = (0f64, 0usize, 0usize);
    for qi in 0..nq {
        let batch = &ps[qi * nd..(qi + 1) * nd];
        let e = encode_pairs(&tok, batch)?;
        let ins = [
            ("input_ids", e.shape, &e.ids[..]),
            ("attention_mask", e.shape, &e.mask[..]),
        ];
        let b = data(burn.forward(
            int2(&e.ids, e.shape, &c.dev),
            int2(&e.mask, e.shape, &c.dev),
        ))
        .data;
        let f = ort_run(&mut f32s, &ins, &[])?.remove(0).data;
        let q = ort_run(&mut q8, &ins, &[])?.remove(0).data;
        worst = worst.max(max_abs_diff(&b, &f));
        top1_f32 += usize::from(argmax(&b) == argmax(&f));
        top1_q8 += usize::from(argmax(&b) == argmax(&q));
    }
    c.line(format!(
        "| Reranker (jina-v1-turbo) | {nq} queries × {nd} docs | max abs logit diff {worst:.2e}; top-1 same as ORT fp32 {top1_f32}/{nq}, as shipped uint8 {top1_q8}/{nq} |"
    ));
    let e = encode_pairs(&tok, &ps[..nd])?;
    let ins = [
        ("input_ids", e.shape, &e.ids[..]),
        ("attention_mask", e.shape, &e.mask[..]),
    ];
    let bt = time(|| {
        burn.forward(
            int2(&e.ids, e.shape, &c.dev),
            int2(&e.mask, e.shape, &c.dev),
        )
        .into_data();
        Ok(())
    })?;
    let ft = time(|| ort_run(&mut f32s, &ins, &[]).map(|_| ()))?;
    let qt = time(|| ort_run(&mut q8, &ins, &[]).map(|_| ()))?;
    c.speed(
        &format!("Reranker, batch of {nd} pairs ({} tokens)", e.shape[1]),
        burn_load,
        f32_load,
        q8_load,
        bt,
        ft,
        qt,
    );
    Ok(())
}

fn nli(c: &mut Ctx) -> Result<()> {
    let dir = c.onnx.join("nli");
    let tok = tokenizer(&dir.join("tokenizer.json"), 512)?;
    let (burn, burn_load) = timed(|| {
        Ok(models::nli::Model::<B64>::from_file(
            c.bpk("nli-fp32", "model"),
            &Default::default(),
        ))
    })?;
    let (mut f32s, f32_load) = timed(|| ort_session(&dir.join("model.onnx")))?;
    let (mut q8, q8_load) = timed(|| ort_session(&dir.join("model_quantized.onnx")))?;
    // Premise = a memory, hypothesis = its own summary (entailment-ish), a
    // negated summary (contradiction-ish) and another memory's summary.
    let mems = &c.ds.memories;
    let mut ps: Vec<(String, String)> = Vec::new();
    for (i, m) in mems.iter().take(10).enumerate() {
        let premise = format!("{} {}", m.summary, m.content);
        ps.push((premise.clone(), m.summary.clone()));
        ps.push((
            premise.clone(),
            format!("It is not true that {}", m.summary.to_lowercase()),
        ));
        ps.push((premise, mems[(i + 7) % mems.len()].summary.clone()));
    }
    let (mut worst, mut same_f32, mut same_q8) = (0f64, 0usize, 0usize);
    // Batch size 1, as production (`classify_one` per pair).
    for (p, h) in &ps {
        let e = encode_pairs(&tok, &[(p.as_str(), h.as_str())])?;
        let ins = [
            ("input_ids", e.shape, &e.ids[..]),
            ("attention_mask", e.shape, &e.mask[..]),
        ];
        let b = data64(burn.forward(int64(&e.ids, e.shape), int64(&e.mask, e.shape))).data;
        let f = ort_run(&mut f32s, &ins, &[])?.remove(0).data;
        let q = ort_run(&mut q8, &ins, &[])?.remove(0).data;
        worst = worst.max(max_abs_diff(&b, &f));
        same_f32 += usize::from(argmax(&b) == argmax(&f));
        same_q8 += usize::from(argmax(&b) == argmax(&q));
    }
    let n = ps.len();
    c.line(format!(
        "| NLI (DeBERTa-v3-xsmall) | {n} pairs | max abs logit diff {worst:.2e}; label same as ORT fp32 {same_f32}/{n}, as shipped int8 {same_q8}/{n} |"
    ));
    let (p, h) = &ps[0];
    let e = encode_pairs(&tok, &[(p.as_str(), h.as_str())])?;
    let ins = [
        ("input_ids", e.shape, &e.ids[..]),
        ("attention_mask", e.shape, &e.mask[..]),
    ];
    let bt = time(|| {
        burn.forward(int64(&e.ids, e.shape), int64(&e.mask, e.shape))
            .into_data();
        Ok(())
    })?;
    let ft = time(|| ort_run(&mut f32s, &ins, &[]).map(|_| ()))?;
    let qt = time(|| ort_run(&mut q8, &ins, &[]).map(|_| ()))?;
    c.speed(
        &format!("NLI (NdArray backend), 1 pair ({} tokens)", e.shape[1]),
        burn_load,
        f32_load,
        q8_load,
        bt,
        ft,
        qt,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T5-small title generation: encoder + greedy decoder, as `title/t5.rs`
// ---------------------------------------------------------------------------

const T5_MAX_IN: usize = 128;
const T5_MAX_OUT: usize = 16;
const T5_EOS: i64 = 1;
const T5_START: i64 = 0;

fn t5_input(tok: &Tokenizer, text: &str) -> Result<Vec<i64>> {
    let e = tok
        .encode(format!("summarize: {text}"), true)
        .map_err(|e| anyhow!("{e}"))?;
    let mut ids: Vec<i64> = e.get_ids().iter().map(|&v| v as i64).collect();
    if ids.len() > T5_MAX_IN {
        ids.truncate(T5_MAX_IN);
        ids[T5_MAX_IN - 1] = T5_EOS;
    }
    Ok(ids)
}

/// Production-style greedy decode with ONNX Runtime: full prefix every step.
fn t5_ort(enc: &mut Session, dec: &mut Session, ids: &[i64]) -> Result<Vec<i64>> {
    let l = ids.len();
    let mask = vec![1i64; l];
    let h = ort_run(
        enc,
        &[
            ("input_ids", [1, l], ids),
            ("attention_mask", [1, l], &mask),
        ],
        &[],
    )?
    .remove(0);
    let mut out = vec![T5_START];
    for _ in 0..T5_MAX_OUT {
        let n = out.len();
        let logits = ort_run(
            dec,
            &[
                ("input_ids", [1, n], &out),
                ("encoder_attention_mask", [1, l], &mask),
            ],
            &[("encoder_hidden_states", &h.shape, &h.data)],
        )?
        .remove(0);
        let v = logits.shape[2];
        let next = argmax(&logits.data[(n - 1) * v..n * v]) as i64;
        if next == T5_EOS {
            break;
        }
        out.push(next);
    }
    Ok(out)
}

/// KV-cached greedy decode with ONNX Runtime (same logic as `t5_burn_kv`),
/// to tell an import bug from a bug in the decode loop.
fn t5_ort_kv(
    enc: &mut Session,
    dec: &mut Session,
    past: &mut Session,
    ids: &[i64],
) -> Result<Vec<i64>> {
    let l = ids.len();
    let mask = vec![1i64; l];
    let h = ort_run(
        enc,
        &[
            ("input_ids", [1, l], ids),
            ("attention_mask", [1, l], &mask),
        ],
        &[],
    )?
    .remove(0);
    let mut first = ort_run(
        dec,
        &[
            ("input_ids", [1, 1], &[T5_START]),
            ("encoder_attention_mask", [1, l], &mask),
        ],
        &[("encoder_hidden_states", &h.shape, &h.data)],
    )?;
    let mut logits = first.remove(0);
    // first = [dec.key, dec.value, enc.key, enc.value] x 6 layers
    let enc_kv: Vec<Out> = first
        .chunks(4)
        .flat_map(|c| {
            [
                Out {
                    shape: c[2].shape.clone(),
                    data: c[2].data.clone(),
                },
                Out {
                    shape: c[3].shape.clone(),
                    data: c[3].data.clone(),
                },
            ]
        })
        .collect();
    let mut dec_kv: Vec<Out> = first
        .chunks(4)
        .flat_map(|c| {
            [
                Out {
                    shape: c[0].shape.clone(),
                    data: c[0].data.clone(),
                },
                Out {
                    shape: c[1].shape.clone(),
                    data: c[1].data.clone(),
                },
            ]
        })
        .collect();
    let mut out = vec![T5_START];
    for _ in 0..T5_MAX_OUT {
        let v = logits.shape[2];
        let n = logits.shape[1];
        let next = argmax(&logits.data[(n - 1) * v..n * v]) as i64;
        if next == T5_EOS {
            break;
        }
        out.push(next);
        let mut names = Vec::new();
        for i in 0..6 {
            for kind in [
                "decoder.key",
                "decoder.value",
                "encoder.key",
                "encoder.value",
            ] {
                names.push(format!("past_key_values.{i}.{kind}"));
            }
        }
        let mut floats: Vec<(&str, &[usize], &[f32])> =
            vec![("encoder_hidden_states", &h.shape, &h.data)];
        for i in 0..6 {
            let d0 = &dec_kv[2 * i];
            let d1 = &dec_kv[2 * i + 1];
            let e0 = &enc_kv[2 * i];
            let e1 = &enc_kv[2 * i + 1];
            floats.push((&names[4 * i], &d0.shape, &d0.data));
            floats.push((&names[4 * i + 1], &d1.shape, &d1.data));
            floats.push((&names[4 * i + 2], &e0.shape, &e0.data));
            floats.push((&names[4 * i + 3], &e1.shape, &e1.data));
        }
        let mut r = ort_run(
            past,
            &[
                ("input_ids", [1, 1], &[next]),
                ("encoder_attention_mask", [1, l], &mask),
            ],
            &floats,
        )?;
        logits = r.remove(0);
        dec_kv = r;
    }
    Ok(out)
}

/// Same decode on the imported Burn models.
fn t5_burn(
    enc: &models::t5_encoder::Model<B>,
    dec: &models::t5_decoder::Model<B>,
    dev: &Dev,
    ids: &[i64],
) -> Vec<i64> {
    let l = ids.len();
    let mask = vec![1i64; l];
    let h = enc.forward(int2(ids, [1, l], dev), int2(&mask, [1, l], dev));
    let mut out = vec![T5_START];
    for _ in 0..T5_MAX_OUT {
        let n = out.len();
        let r = dec.forward(int2(&mask, [1, l], dev), int2(&out, [1, n], dev), h.clone());
        let logits = data(r.0);
        let v = logits.shape[2];
        let next = argmax(&logits.data[(n - 1) * v..n * v]) as i64;
        if next == T5_EOS {
            break;
        }
        out.push(next);
    }
    out
}

/// KV-cached decode on Burn: `decoder_model` for the first step (it returns
/// the cache), then `decoder_with_past_model` for one token per step.
fn t5_burn_kv(
    enc: &models::t5_encoder::Model<B>,
    dec: &models::t5_decoder::Model<B>,
    past: &models::t5_decoder_past::Model<B>,
    dev: &Dev,
    ids: &[i64],
) -> Vec<i64> {
    let l = ids.len();
    let mask = vec![1i64; l];
    let h = enc.forward(int2(ids, [1, l], dev), int2(&mask, [1, l], dev));
    let r = dec.forward(
        int2(&mask, [1, l], dev),
        int2(&[T5_START], [1, 1], dev),
        h.clone(),
    );
    let (logits, kv) = split_first_step(r);
    let mut dec_kv: Vec<Tensor<B, 4>> = Vec::new();
    let mut enc_kv: Vec<Tensor<B, 4>> = Vec::new();
    for layer in kv.chunks(4) {
        dec_kv.extend([layer[0].clone(), layer[1].clone()]);
        enc_kv.extend([layer[2].clone(), layer[3].clone()]);
    }
    let mut out = vec![T5_START];
    let mut logits = data(logits);
    for _ in 0..T5_MAX_OUT {
        let v = logits.shape[2];
        let n = logits.shape[1];
        let next = argmax(&logits.data[(n - 1) * v..n * v]) as i64;
        if next == T5_EOS {
            break;
        }
        out.push(next);
        let d = &dec_kv;
        let e = &enc_kv;
        let r = past.forward(
            int2(&mask, [1, l], dev),
            int2(&[next], [1, 1], dev),
            h.clone(),
            d[0].clone(),
            d[1].clone(),
            e[0].clone(),
            e[1].clone(),
            d[2].clone(),
            d[3].clone(),
            e[2].clone(),
            e[3].clone(),
            d[4].clone(),
            d[5].clone(),
            e[4].clone(),
            e[5].clone(),
            d[6].clone(),
            d[7].clone(),
            e[6].clone(),
            e[7].clone(),
            d[8].clone(),
            d[9].clone(),
            e[8].clone(),
            e[9].clone(),
            d[10].clone(),
            d[11].clone(),
            e[10].clone(),
            e[11].clone(),
        );
        logits = data(r.0);
        dec_kv = vec![
            r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11, r.12,
        ];
    }
    out
}

type FirstStep = (
    Tensor<B, 3>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
    Tensor<B, 4>,
);

/// `(logits, [dec.key, dec.value, enc.key, enc.value] × 6 layers)`.
fn split_first_step(r: FirstStep) -> (Tensor<B, 3>, Vec<Tensor<B, 4>>) {
    (
        r.0,
        vec![
            r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11, r.12, r.13, r.14, r.15, r.16,
            r.17, r.18, r.19, r.20, r.21, r.22, r.23, r.24,
        ],
    )
}

fn t5(c: &mut Ctx) -> Result<()> {
    let dir = c.onnx.join("t5");
    let mut tok = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
    tok.with_truncation(None).map_err(|e| anyhow!("{e}"))?;
    let ((enc_b, dec_b, past_b), burn_load) = timed(|| {
        Ok((
            models::t5_encoder::Model::<B>::from_file(
                c.bpk("t5-encoder-fp32", "encoder_model"),
                &c.dev,
            ),
            models::t5_decoder::Model::<B>::from_file(
                c.bpk("t5-decoder-fp32", "decoder_model"),
                &c.dev,
            ),
            models::t5_decoder_past::Model::<B>::from_file(
                c.bpk("t5-decoder-past-fp32", "decoder_with_past_model"),
                &c.dev,
            ),
        ))
    })?;
    let mut past_f = ort_session(&dir.join("decoder_with_past_model.onnx"))?;
    let ((mut enc_f, mut dec_f), f32_load) = timed(|| {
        Ok((
            ort_session(&dir.join("encoder_model.onnx"))?,
            ort_session(&dir.join("decoder_model.onnx"))?,
        ))
    })?;
    let ((mut enc_q, mut dec_q), q8_load) = timed(|| {
        Ok((
            ort_session(&dir.join("encoder_model_quantized.onnx"))?,
            ort_session(&dir.join("decoder_model_quantized.onnx"))?,
        ))
    })?;

    // Numeric check on the encoder and one decoder step.
    let docs = c.docs();
    let ids = t5_input(&tok, &docs[0])?;
    let l = ids.len();
    let mask = vec![1i64; l];
    let hb = data(enc_b.forward(int2(&ids, [1, l], &c.dev), int2(&mask, [1, l], &c.dev)));
    let hf = ort_run(
        &mut enc_f,
        &[
            ("input_ids", [1, l], &ids),
            ("attention_mask", [1, l], &mask),
        ],
        &[],
    )?
    .remove(0);
    let enc_diff = max_abs_diff(&hb.data, &hf.data);
    // Feed ORT's encoder output to both decoders so only the decoder differs.
    let prefix = [T5_START, 363, 3];
    let h_t: Tensor<B, 3> = Tensor::from_data(
        TensorData::new(hf.data.clone(), [1, l, hf.shape[2]]),
        &c.dev,
    );
    let db = data(
        dec_b
            .forward(
                int2(&mask, [1, l], &c.dev),
                int2(&prefix, [1, 3], &c.dev),
                h_t,
            )
            .0,
    );
    let df = ort_run(
        &mut dec_f,
        &[
            ("input_ids", [1, 3], &prefix),
            ("encoder_attention_mask", [1, l], &mask),
        ],
        &[("encoder_hidden_states", &hf.shape, &hf.data)],
    )?
    .remove(0);
    let dec_diff = max_abs_diff(&db.data, &df.data);

    let n = 10;
    let (mut same_f, mut same_q, mut same_kv, mut ort_kv_ok) = (0, 0, 0, 0);
    let mut examples = Vec::new();
    for d in docs.iter().take(n) {
        let ids = t5_input(&tok, d)?;
        let b = t5_burn(&enc_b, &dec_b, &c.dev, &ids);
        let kv = t5_burn_kv(&enc_b, &dec_b, &past_b, &c.dev, &ids);
        let f = t5_ort(&mut enc_f, &mut dec_f, &ids)?;
        let q = t5_ort(&mut enc_q, &mut dec_q, &ids)?;
        same_f += usize::from(b == f);
        same_q += usize::from(b == q);
        same_kv += usize::from(kv == b);
        let okv = t5_ort_kv(&mut enc_f, &mut dec_f, &mut past_f, &ids)?;
        ort_kv_ok += usize::from(okv == f);
        if kv != b && examples.len() < 3 {
            let dec = |v: &[i64]| {
                tok.decode(&v.iter().map(|&x| x as u32).collect::<Vec<_>>(), true)
                    .unwrap_or_default()
            };
            eprintln!(
                "KV mismatch: full \"{}\" / Burn KV \"{}\" / ORT KV \"{}\"",
                dec(&b),
                dec(&kv),
                dec(&okv)
            );
        }
        if examples.len() < 3 {
            let dec = |v: &[i64]| {
                tok.decode(&v.iter().map(|&x| x as u32).collect::<Vec<_>>(), true)
                    .unwrap_or_default()
            };
            examples.push(format!("Burn: \"{}\" / ORT int8: \"{}\"", dec(&b), dec(&q)));
        }
    }
    c.line(format!(
        "| T5-small titles | {n} memories | encoder max abs diff {enc_diff:.2e}, decoder logits {dec_diff:.2e}; title same as ORT fp32 {same_f}/{n}, as shipped int8 {same_q}/{n}; Burn KV-cache decode same as full-prefix {same_kv}/{n} (ORT KV-cache same as ORT full-prefix {ort_kv_ok}/{n}) |"
    ));
    for e in examples {
        c.line(format!("|  | example | {e} |"));
    }
    let ids = t5_input(&tok, &docs[1])?;
    let bt = time(|| {
        t5_burn(&enc_b, &dec_b, &c.dev, &ids);
        Ok(())
    })?;
    let bkv = time(|| {
        t5_burn_kv(&enc_b, &dec_b, &past_b, &c.dev, &ids);
        Ok(())
    })?;
    let ft = time(|| t5_ort(&mut enc_f, &mut dec_f, &ids).map(|_| ()))?;
    let qt = time(|| t5_ort(&mut enc_q, &mut dec_q, &ids).map(|_| ()))?;
    let what = format!("T5 title, {} input tokens (full-prefix decode)", ids.len());
    c.speed(&what, burn_load, f32_load, q8_load, bt, ft, qt);
    c.speed(
        "T5 title, Burn with KV-cache decode",
        burn_load,
        f32_load,
        q8_load,
        bkv,
        ft,
        qt,
    );
    Ok(())
}

// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let ds: Dataset = serde_json::from_str(&std::fs::read_to_string(
        root.join("../../examples/data/embed_eval.json"),
    )?)?;
    let mut c = Ctx {
        dev: Default::default(),
        gen: root.join("gen"),
        onnx: root.join("../inference-engines/models/onnx-import"),
        ds,
        report: String::new(),
        speed_rows: Vec::new(),
    };
    let only = std::env::args().nth(1);
    let want = |m: &str| only.as_deref().is_none_or(|o| o == m);
    c.line("| Model | Inputs | Agreement (Burn imported fp32 vs ONNX Runtime) |".into());
    c.line("|---|---|---|".into());
    if want("minilm") {
        minilm(&mut c)?;
    }
    if want("reranker") {
        reranker(&mut c)?;
    }
    if want("nli") {
        nli(&mut c)?;
    }
    if want("t5") {
        t5(&mut c)?;
    }
    let mut md = std::mem::take(&mut c.report);
    writeln!(md, "\n| Workload | Load ms (Burn / ORT fp32 / ORT shipped) | Burn p50 ms | ORT fp32 p50 ms | ORT shipped p50 ms | Burn vs ORT fp32 | Burn vs ORT shipped |")?;
    writeln!(md, "|---|---:|---:|---:|---:|---:|---:|")?;
    for r in &c.speed_rows {
        writeln!(md, "{r}")?;
    }
    print!("{md}");
    Ok(())
}
