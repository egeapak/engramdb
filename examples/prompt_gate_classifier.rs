//! Viability test: a small trained classifier for the context gate.
//!
//! The gate decides whether the UserPromptSubmit hook searches memories for
//! a turn. `examples/prompt_turn_eval.rs` showed that the prompt alone cannot
//! decide it ("yes" after "Should I push?" asks for work, "yes" after "Done"
//! does not) and that zero-shot NLI and Laya do not read the previous message
//! well enough. This example trains a logistic regression on MiniLM
//! embeddings of the prompt and of the agent's previous message.
//!
//! - Training data: `examples/data/prompt_turn_train.json` (360 pairs,
//!   written by a separate model run that never saw the evaluation set).
//! - Evaluation data: `examples/data/prompt_turn_eval.json` (60 held-out
//!   pairs, the same set as `prompt_turn_eval.rs`).
//! - The L2 strength and the skip threshold are chosen by 5-fold
//!   cross-validation on the training set only; the evaluation set is scored
//!   once with the chosen values.
//!
//! Run: `cargo run --release --example prompt_gate_classifier`
//! (needs the default ONNX embedding model in the model cache and an ONNX
//! Runtime shared library; see CLAUDE.md).

use std::time::Instant;

use anyhow::{ensure, Context, Result};
use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};
use serde::Deserialize;

#[derive(Deserialize)]
struct DataFile {
    items: Vec<Item>,
}

#[derive(Deserialize, Clone)]
struct Item {
    last: String,
    prompt: String,
    search: bool,
    #[serde(default)]
    tags: Vec<String>,
}

/// Characters kept from the end of the previous message. Agent messages put
/// the question or proposal last, and MiniLM truncates from the end of its
/// input, so the tail is what must survive.
const LAST_TAIL_CHARS: usize = 300;

/// The largest share of "search" items a threshold may wrongly skip in
/// cross-validation. A wrong skip hides memories the user needed. Raised from
/// 2% after the first cross-validation run, where only settings that never
/// skip met 2%.
const MAX_CV_WRONG_SKIP_RATE: f64 = 0.05;

/// L2 strengths. The values above 0.1 were added after the first
/// cross-validation run showed every model memorizing the training set.
const L2_GRID: &[f32] = &[1e-3, 1e-1, 1.0, 3.0, 10.0, 30.0];
const THRESHOLD_GRID: &[f32] = &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
const FOLDS: usize = 5;

const EXTRA_TEST_SETS: &[&str] = &[
    "prompt_turn_test_realistic.json",
    "prompt_turn_test_hard.json",
];

fn tail(text: &str, max_chars: usize) -> &str {
    let count = text.chars().count();
    if count <= max_chars {
        return text;
    }
    let start = text
        .char_indices()
        .nth(count - max_chars)
        .map_or(0, |(i, _)| i);
    let cut = &text[start..];
    // Start at a word boundary so the first token is not a word fragment.
    cut.find(char::is_whitespace)
        .map_or(cut, |i| cut[i..].trim_start())
}

#[derive(Clone, Copy, PartialEq)]
enum Features {
    /// Prompt embedding only.
    PromptOnly,
    /// Prompt and previous-message embeddings, concatenated.
    Concat,
    /// Prompt, previous message, and their elementwise product.
    ConcatProduct,
    /// One embedding of the joined text "Assistant: … User: …", so the
    /// encoder itself relates the reply to the message it answers. Added
    /// after the first cross-validation run.
    Joint,
    /// The joint embedding plus the prompt embedding.
    JointPrompt,
}

impl Features {
    fn name(self) -> &'static str {
        match self {
            Features::PromptOnly => "prompt only",
            Features::Concat => "prompt + previous message",
            Features::ConcatProduct => "prompt + previous message + product",
            Features::Joint => "joint text",
            Features::JointPrompt => "joint text + prompt",
        }
    }

    fn build(self, (p, l, j): &Embedded) -> Vec<f32> {
        match self {
            Features::Joint => j.clone(),
            Features::JointPrompt => j.iter().chain(p).copied().collect(),
            Features::PromptOnly => p.to_vec(),
            Features::Concat => p.iter().chain(l.iter()).copied().collect(),
            Features::ConcatProduct => p
                .iter()
                .chain(l.iter())
                .copied()
                .chain(p.iter().zip(l.iter()).map(|(a, b)| a * b))
                .collect(),
        }
    }
}

/// Logistic regression on standardized features,
/// P(search) = sigmoid(w·z + b) with z = (x - mean) / std.
struct Model {
    mean: Vec<f32>,
    inv_std: Vec<f32>,
    w: Vec<f32>,
    b: f32,
}

fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + (-z).exp())
}

impl Model {
    /// Full-batch gradient descent from zero weights: deterministic.
    /// Classes are weighted so each contributes equally to the loss.
    fn train(raw: &[Vec<f32>], ys: &[bool], l2: f32) -> Self {
        let dim = raw[0].len();
        let n = raw.len() as f32;
        // Standardize with the training rows' own statistics, so a held-out
        // fold never informs them.
        let mut mean = vec![0f32; dim];
        for x in raw {
            for (m, v) in mean.iter_mut().zip(x) {
                *m += v / n;
            }
        }
        let mut var = vec![0f32; dim];
        for x in raw {
            for ((s, v), m) in var.iter_mut().zip(x).zip(&mean) {
                *s += (v - m) * (v - m) / n;
            }
        }
        let inv_std: Vec<f32> = var.iter().map(|v| 1.0 / (v.sqrt() + 1e-6)).collect();
        let xs: Vec<Vec<f32>> = raw
            .iter()
            .map(|x| standardize(x, &mean, &inv_std))
            .collect();
        let pos = ys.iter().filter(|y| **y).count() as f32;
        let (w_pos, w_neg) = (n / (2.0 * pos), n / (2.0 * (n - pos)));
        let mut w = vec![0f32; dim];
        let mut b = 0f32;
        let lr = 0.05;
        for _ in 0..1500 {
            let mut gw = vec![0f32; dim];
            let mut gb = 0f32;
            for (x, &y) in xs.iter().zip(ys) {
                let p = sigmoid(dot(&w, x) + b);
                let weight = if y { w_pos } else { w_neg };
                let err = weight * (p - if y { 1.0 } else { 0.0 });
                for (g, xi) in gw.iter_mut().zip(x) {
                    *g += err * xi;
                }
                gb += err;
            }
            for (wi, g) in w.iter_mut().zip(&gw) {
                *wi -= lr * (g / n + l2 * *wi);
            }
            b -= lr * gb / n;
        }
        Model {
            mean,
            inv_std,
            w,
            b,
        }
    }

    fn prob(&self, x: &[f32]) -> f32 {
        sigmoid(dot(&self.w, &standardize(x, &self.mean, &self.inv_std)) + self.b)
    }
}

fn standardize(x: &[f32], mean: &[f32], inv_std: &[f32]) -> Vec<f32> {
    x.iter()
        .zip(mean)
        .zip(inv_std)
        .map(|((v, m), s)| (v - m) * s)
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[derive(Default, Clone, Copy)]
struct Counts {
    correct: usize,
    total: usize,
    wrong_skips: usize,
    searches: usize,
    skips_caught: usize,
    skips: usize,
}

impl Counts {
    fn add(&mut self, gold: bool, pred: bool) {
        self.total += 1;
        self.correct += usize::from(gold == pred);
        if gold {
            self.searches += 1;
            self.wrong_skips += usize::from(!pred);
        } else {
            self.skips += 1;
            self.skips_caught += usize::from(!pred);
        }
    }

    fn accuracy(&self) -> f64 {
        100.0 * self.correct as f64 / self.total.max(1) as f64
    }

    fn wrong_skip_rate(&self) -> f64 {
        self.wrong_skips as f64 / self.searches.max(1) as f64
    }

    fn line(&self) -> String {
        format!(
            "acc {:>5.1}%  skips caught {:>3}/{:<3}  wrong skips {:>2}/{}",
            self.accuracy(),
            self.skips_caught,
            self.skips,
            self.wrong_skips,
            self.searches
        )
    }
}

/// Out-of-fold P(search) for every training item. Folds are assigned by
/// index modulo `FOLDS`; contrast pairs written next to each other then fall
/// in different folds, which makes cross-validation harder, not easier.
fn out_of_fold(xs: &[Vec<f32>], ys: &[bool], l2: f32) -> Vec<f32> {
    let mut probs = vec![0f32; xs.len()];
    for fold in 0..FOLDS {
        let (mut tx, mut ty) = (Vec::new(), Vec::new());
        for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
            if i % FOLDS != fold {
                tx.push(x.clone());
                ty.push(*y);
            }
        }
        let m = Model::train(&tx, &ty, l2);
        for (i, x) in xs.iter().enumerate() {
            if i % FOLDS == fold {
                probs[i] = m.prob(x);
            }
        }
    }
    probs
}

fn counts_at(probs: &[f32], ys: &[bool], threshold: f32) -> Counts {
    let mut c = Counts::default();
    for (p, y) in probs.iter().zip(ys) {
        c.add(*y, *p >= threshold);
    }
    c
}

fn load(name: &str) -> Result<Vec<Item>> {
    let path = format!("{}/examples/data/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    Ok(serde_json::from_str::<DataFile>(&raw)?.items)
}

/// The joined text for [`Features::Joint`].
fn joint_text(item: &Item) -> String {
    format!(
        "Assistant: {}\nUser: {}",
        tail(&item.last, LAST_TAIL_CHARS),
        item.prompt
    )
}

type Embedded = (Vec<f32>, Vec<f32>, Vec<f32>);

/// (prompt, previous-message tail, joint text) embeddings per item.
async fn embed_all(provider: &OnnxProvider, items: &[Item]) -> Result<Vec<Embedded>> {
    let prompts: Vec<&str> = items.iter().map(|i| i.prompt.as_str()).collect();
    let lasts: Vec<&str> = items
        .iter()
        .map(|i| tail(&i.last, LAST_TAIL_CHARS))
        .collect();
    let joints: Vec<String> = items.iter().map(joint_text).collect();
    let joint_refs: Vec<&str> = joints.iter().map(String::as_str).collect();
    let p = provider.embed_batch(&prompts).await?;
    let l = provider.embed_batch(&lasts).await?;
    let j = provider.embed_batch(&joint_refs).await?;
    ensure!(p.len() == items.len() && l.len() == items.len() && j.len() == items.len());
    Ok(p.into_iter()
        .zip(l)
        .zip(j)
        .map(|((p, l), j)| (p, l, j))
        .collect())
}

#[tokio::main]
async fn main() -> Result<()> {
    let train = load("prompt_turn_train.json")?;
    let eval = load("prompt_turn_eval.json")?;
    // Larger test sets written by separate model runs that saw no other
    // data file. Scored when present.
    let mut extra = Vec::new();
    for name in EXTRA_TEST_SETS {
        let path = format!("{}/examples/data/{name}", env!("CARGO_MANIFEST_DIR"));
        if std::path::Path::new(&path).exists() {
            extra.push((*name, load(name)?));
        }
    }

    // A training pair that duplicates an evaluation pair would leak.
    let overlap = train
        .iter()
        .filter(|t| {
            eval.iter()
                .any(|e| e.prompt == t.prompt && e.last == t.last)
        })
        .count();
    let same_prompt = eval
        .iter()
        .filter(|e| {
            train
                .iter()
                .any(|t| t.prompt.eq_ignore_ascii_case(&e.prompt))
        })
        .count();

    let provider = OnnxProvider::new().context("default ONNX embedding model did not load")?;
    let t = Instant::now();
    let train_emb = embed_all(&provider, &train).await?;
    let eval_emb = embed_all(&provider, &eval).await?;
    let mut extra_emb = Vec::new();
    for (_, items) in &extra {
        extra_emb.push(embed_all(&provider, items).await?);
    }
    let embed_ms = t.elapsed().as_secs_f64() * 1e3;

    let train_y: Vec<bool> = train.iter().map(|i| i.search).collect();
    let eval_y: Vec<bool> = eval.iter().map(|i| i.search).collect();

    println!(
        "Gate classifier: {} training pairs ({} search / {} skip), {} evaluation pairs",
        train.len(),
        train_y.iter().filter(|y| **y).count(),
        train_y.iter().filter(|y| !**y).count(),
        eval.len()
    );
    println!(
        "  identical (last, prompt) pairs shared with the evaluation set: {overlap}; \
         evaluation prompts whose text also appears in training: {same_prompt}/{}",
        eval.len()
    );
    println!(
        "  embedded {} texts in {embed_ms:.0} ms",
        3 * (train.len() + eval.len())
    );
    println!("  baseline on evaluation, always search (shipped): {}", {
        let mut c = Counts::default();
        for y in &eval_y {
            c.add(*y, true);
        }
        c.line()
    });

    for features in [
        Features::PromptOnly,
        Features::Concat,
        Features::ConcatProduct,
        Features::Joint,
        Features::JointPrompt,
    ] {
        let xs: Vec<Vec<f32>> = train_emb.iter().map(|e| features.build(e)).collect();
        let ex: Vec<Vec<f32>> = eval_emb.iter().map(|e| features.build(e)).collect();

        // Model selection on the training set only.
        let mut best: Option<(f32, f32, Counts)> = None;
        for &l2 in L2_GRID {
            let oof = out_of_fold(&xs, &train_y, l2);
            for &t in THRESHOLD_GRID {
                let c = counts_at(&oof, &train_y, t);
                if std::env::var_os("GATE_CV_ONLY").is_some() {
                    println!(
                        "  [{}] cv l2 {l2:e} t {t:.1}: {}",
                        features.name(),
                        c.line()
                    );
                }
                if c.wrong_skip_rate() > MAX_CV_WRONG_SKIP_RATE {
                    continue;
                }
                let better = best.is_none_or(|(_, _, b)| c.correct > b.correct);
                if better {
                    best = Some((l2, t, c));
                }
            }
        }

        if std::env::var_os("GATE_CV_ONLY").is_some() {
            for &l2 in L2_GRID {
                let m = Model::train(&xs, &train_y, l2);
                let probs: Vec<f32> = xs.iter().map(|x| m.prob(x)).collect();
                println!(
                    "  [{}] fit on training set, l2 {l2:e} t 0.5: {}",
                    features.name(),
                    counts_at(&probs, &train_y, 0.5).line()
                );
            }
            continue;
        }
        println!("\n{} ({} features)", features.name(), xs[0].len());
        let Some((l2, threshold, cv)) = best else {
            println!(
                "  no setting keeps cross-validated wrong skips at or below {:.0}%",
                MAX_CV_WRONG_SKIP_RATE * 100.0
            );
            continue;
        };
        println!("  chosen by 5-fold CV: l2 {l2:e}, skip when P(search) < {threshold:.1}");
        println!("  cross-validation (training set): {}", cv.line());

        let t = Instant::now();
        let model = Model::train(&xs, &train_y, l2);
        let train_ms = t.elapsed().as_secs_f64() * 1e3;
        let probs: Vec<f32> = ex.iter().map(|x| model.prob(x)).collect();
        let c = counts_at(&probs, &eval_y, threshold);
        println!("  held-out evaluation:             {}", c.line());
        for ((name, items), emb) in extra.iter().zip(&extra_emb) {
            let mut c = Counts::default();
            for (item, e) in items.iter().zip(emb) {
                c.add(item.search, model.prob(&features.build(e)) >= threshold);
            }
            println!("  {name:<31}  {}", c.line());
        }
        println!(
            "  training took {train_ms:.0} ms; the model is {} weights ({} bytes as f32)",
            model.w.len() + 1,
            4 * (model.w.len() + 1)
        );

        let mut tags: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
        for ((item, p), y) in eval.iter().zip(&probs).zip(&eval_y) {
            for tag in &item.tags {
                let e = tags.entry(tag.as_str()).or_default();
                e.1 += 1;
                e.0 += usize::from((*p >= threshold) == *y);
            }
        }
        let tag_line: Vec<String> = tags
            .iter()
            .map(|(t, (c, n))| format!("{t} {c}/{n}"))
            .collect();
        println!("  by tag: {}", tag_line.join(", "));

        print!("  threshold sweep on evaluation:");
        for &t in THRESHOLD_GRID {
            let c = counts_at(&probs, &eval_y, t);
            print!(
                "  {t:.1}: {}/{} wrong {}",
                c.correct, c.total, c.wrong_skips
            );
        }
        println!();

        if features != Features::PromptOnly {
            println!("  errors at the chosen threshold:");
            for ((item, p), y) in eval.iter().zip(&probs).zip(&eval_y) {
                if (*p >= threshold) != *y {
                    println!(
                        "    gold {:<6} P(search) {:.2}  {:<28} | {}",
                        if *y { "search" } else { "skip" },
                        p,
                        item.prompt,
                        tail(&item.last, 70)
                    );
                }
            }
        }
    }
    Ok(())
}
