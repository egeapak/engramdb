//! Engine-independent pieces: tokenization, pooling, the corpus, timing and
//! memory readings. Every engine gets byte-identical token ids from here and
//! returns raw `last_hidden_state`; pooling and normalization happen once, in
//! this file, so engines differ only in the forward pass.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// EngramDB's `[embeddings].max_tokens` default for the MiniLM specs.
pub const MAX_TOKENS: usize = 256;
pub const HIDDEN: usize = 384;

/// One padded batch, row-major `[batch, seq]`.
pub struct Batch {
    pub batch: usize,
    pub seq: usize,
    pub input_ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    pub token_type_ids: Vec<i64>,
}

/// The forward pass is the only thing an engine provides.
pub trait Engine {
    /// Returns `last_hidden_state` flattened as `[batch, seq, HIDDEN]`.
    fn forward(&mut self, batch: &Batch) -> Result<Vec<f32>>;
}

/// Same settings fastembed applies in production: truncate at `MAX_TOKENS`,
/// pad to the longest sequence in the batch (the file's own `Fixed(128)`
/// padding is replaced).
pub fn load_tokenizer(path: &Path) -> Result<Tokenizer> {
    let mut tok = Tokenizer::from_file(path).map_err(|e| anyhow!("tokenizer: {e}"))?;
    tok.with_truncation(Some(TruncationParams {
        max_length: MAX_TOKENS,
        ..Default::default()
    }))
    .map_err(|e| anyhow!("truncation: {e}"))?;
    tok.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        ..Default::default()
    }));
    Ok(tok)
}

pub fn encode(tok: &Tokenizer, texts: &[&str]) -> Result<Batch> {
    let enc = tok
        .encode_batch(texts.to_vec(), true)
        .map_err(|e| anyhow!("encode: {e}"))?;
    let batch = enc.len();
    let seq = enc.first().map(|e| e.len()).unwrap_or(0);
    let mut b = Batch {
        batch,
        seq,
        input_ids: Vec::with_capacity(batch * seq),
        attention_mask: Vec::with_capacity(batch * seq),
        token_type_ids: Vec::with_capacity(batch * seq),
    };
    for e in &enc {
        b.input_ids.extend(e.get_ids().iter().map(|&x| x as i64));
        b.attention_mask
            .extend(e.get_attention_mask().iter().map(|&x| x as i64));
        b.token_type_ids
            .extend(e.get_type_ids().iter().map(|&x| x as i64));
    }
    Ok(b)
}

/// Masked mean pooling + L2 normalization (what fastembed does for MiniLM).
pub fn pool(hidden: &[f32], b: &Batch) -> Vec<Vec<f32>> {
    assert_eq!(hidden.len(), b.batch * b.seq * HIDDEN, "hidden-state shape");
    (0..b.batch)
        .map(|i| {
            let mut v = vec![0f32; HIDDEN];
            let mut n = 0f32;
            for t in 0..b.seq {
                if b.attention_mask[i * b.seq + t] == 0 {
                    continue;
                }
                n += 1.0;
                let row = &hidden[(i * b.seq + t) * HIDDEN..][..HIDDEN];
                for (a, x) in v.iter_mut().zip(row) {
                    *a += x;
                }
            }
            let n = n.max(1e-9);
            v.iter_mut().for_each(|a| *a /= n);
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            v.iter_mut().for_each(|a| *a /= norm);
            v
        })
        .collect()
}

pub fn embed(engine: &mut dyn Engine, tok: &Tokenizer, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    let b = encode(tok, texts)?;
    let hidden = engine.forward(&b)?;
    Ok(pool(&hidden, &b))
}

// ---------------------------------------------------------------------------
// Corpus: EngramDB's own retrieval eval set (examples/data/embed_eval.json).
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Dataset {
    pub memories: Vec<Memory>,
    pub queries: Vec<Query>,
}

#[derive(Deserialize)]
pub struct Memory {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct Query {
    pub id: String,
    pub text: String,
    pub relevant: BTreeMap<String, u8>,
}

impl Memory {
    /// One document per memory. Truncation at `MAX_TOKENS` happens in the
    /// tokenizer, the same for every engine.
    pub fn doc(&self) -> String {
        format!("{}. {} {}", self.title, self.summary, self.content)
    }
}

pub fn load_dataset(path: &Path) -> Result<Dataset> {
    let s = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&s)?)
}

// ---------------------------------------------------------------------------
// Measurement helpers.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Latency {
    pub n: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

impl Latency {
    pub fn from_samples(mut ms: Vec<f64>) -> Self {
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = ms.len();
        if n == 0 {
            return Self::default();
        }
        let pct = |p: f64| ms[((n - 1) as f64 * p).round() as usize];
        Self {
            n,
            mean_ms: ms.iter().sum::<f64>() / n as f64,
            p50_ms: pct(0.50),
            p95_ms: pct(0.95),
            min_ms: ms[0],
            max_ms: ms[n - 1],
        }
    }
}

/// `(VmRSS, VmHWM)` in MiB from `/proc/self/status` (Linux only; zeros
/// elsewhere). VmHWM is the peak resident set since process start.
pub fn rss_mib() -> (f64, f64) {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return (0.0, 0.0);
    };
    let field = |name: &str| {
        s.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<f64>().ok())
            .map(|kb| kb / 1024.0)
            .unwrap_or(0.0)
    };
    (field("VmRSS:"), field("VmHWM:"))
}

/// Process CPU time (user + system) in seconds, from `/proc/self/stat`.
/// Divided by wall time it gives the number of cores an engine kept busy.
pub fn cpu_secs() -> f64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    // Fields after the parenthesised command name; utime/stime are 14 and 15.
    let rest = s.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    // Linux USER_HZ is 100 on every mainstream configuration.
    (ticks(11) + ticks(12)) / 100.0
}

pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    dot / (na * nb).max(1e-12)
}
