//! Evaluate the prompt-intent prototype against the shipped baselines.
//!
//! Two decisions the UserPromptSubmit hook makes (see `src/prompt_intent.rs`):
//!
//! 1. **Situation** (debugging / design choice / none). Baseline: the shipped
//!    keyword tables (`keyword_situation`). Candidate: the nearest-seed
//!    `PrototypeClassifier` over the default ONNX embedding model.
//! 2. **Context gate** (search, or skip a turn-control prompt). Baselines:
//!    the shipped behavior (always search) and a word-list rule written for
//!    this evaluation (skip when every word is a turn-control word).
//!    Candidate: the prototype's gate.
//!
//! Data: `examples/data/prompt_intent_eval.json` (150 hand-labelled prompts).
//! The same author wrote the seeds, the word list and the labels, so treat
//! the absolute numbers as optimistic; the comparison between methods on the
//! tagged subsets is the useful signal.
//!
//! Run: `cargo run --release --example prompt_intent_eval`
//! (needs the default ONNX embedding model in the model cache and an ONNX
//! Runtime shared library; see CLAUDE.md).

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::{EmbeddingProvider, OnnxProvider};
use engramdb::prompt_intent::{keyword_situation, PrototypeClassifier, PrototypeParams};
use engramdb::types::Situation;
use serde::Deserialize;

#[derive(Deserialize)]
struct EvalFile {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    prompt: String,
    situation: String,
    needs_context: bool,
    #[serde(default)]
    tags: Vec<String>,
}

const LABELS: [&str; 3] = ["debugging", "design_choice", "none"];

fn label(s: Option<Situation>) -> &'static str {
    match s {
        Some(Situation::Debugging) => "debugging",
        Some(Situation::DesignChoice) => "design_choice",
        _ => "none",
    }
}

/// Turn-control words for the word-list gate baseline. Not used by the
/// product; it shows how far a rule with no model gets.
const TURN_CONTROL_WORDS: &[&str] = &[
    "ok", "okay", "k", "thanks", "thank", "you", "so", "much", "yes", "y", "yep", "yeah", "sure",
    "thing", "continue", "go", "ahead", "on", "proceed", "lgtm", "great", "good", "nice", "cool",
    "awesome", "perfect", "fine", "by", "me", "alright", "do", "it", "that", "please", "looks",
    "job", "carry", "next", "approved", "hmm", "makes", "sense", "no", "stop", "wait",
];

fn word_list_needs_context(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    words.is_empty() || !words.iter().all(|w| TURN_CONTROL_WORDS.contains(w))
}

/// Accuracy, macro-F1 and a confusion matrix for one situation predictor.
struct SituationReport {
    correct: usize,
    total: usize,
    /// confusion[gold][predicted]
    confusion: BTreeMap<&'static str, BTreeMap<&'static str, usize>>,
    by_tag: BTreeMap<String, (usize, usize)>,
}

impl SituationReport {
    fn new() -> Self {
        Self {
            correct: 0,
            total: 0,
            confusion: BTreeMap::new(),
            by_tag: BTreeMap::new(),
        }
    }

    fn add(&mut self, item: &Item, gold: &'static str, pred: &'static str) {
        self.total += 1;
        let hit = gold == pred;
        if hit {
            self.correct += 1;
        }
        *self
            .confusion
            .entry(gold)
            .or_default()
            .entry(pred)
            .or_default() += 1;
        for tag in &item.tags {
            let e = self.by_tag.entry(tag.clone()).or_default();
            e.1 += 1;
            if hit {
                e.0 += 1;
            }
        }
    }

    fn cell(&self, gold: &str, pred: &str) -> usize {
        self.confusion
            .get(gold)
            .and_then(|m| m.get(pred))
            .copied()
            .unwrap_or(0)
    }

    fn macro_f1(&self) -> f64 {
        let mut sum = 0.0;
        for l in LABELS {
            let tp = self.cell(l, l) as f64;
            let fp: f64 = LABELS
                .iter()
                .filter(|g| **g != l)
                .map(|g| self.cell(g, l) as f64)
                .sum();
            let fn_: f64 = LABELS
                .iter()
                .filter(|p| **p != l)
                .map(|p| self.cell(l, p) as f64)
                .sum();
            let p = if tp + fp > 0.0 { tp / (tp + fp) } else { 0.0 };
            let r = if tp + fn_ > 0.0 { tp / (tp + fn_) } else { 0.0 };
            sum += if p + r > 0.0 {
                2.0 * p * r / (p + r)
            } else {
                0.0
            };
        }
        sum / LABELS.len() as f64
    }

    fn print(&self, name: &str) {
        println!(
            "  {name:<22} accuracy {:>5.1}%  ({}/{})  macro-F1 {:.3}",
            pct(self.correct, self.total),
            self.correct,
            self.total,
            self.macro_f1()
        );
        println!(
            "    confusion (rows = gold, columns = predicted: debugging / design_choice / none)"
        );
        for g in LABELS {
            println!(
                "      {g:<14} {:>4} {:>4} {:>4}",
                self.cell(g, "debugging"),
                self.cell(g, "design_choice"),
                self.cell(g, "none")
            );
        }
        let tags: Vec<String> = self
            .by_tag
            .iter()
            .map(|(t, (c, n))| format!("{t} {c}/{n}"))
            .collect();
        println!("    by tag: {}", tags.join(", "));
    }
}

/// Gate outcome counts. A wrong skip hides memories the user needed; a
/// missed skip only costs one unneeded search, as today.
#[derive(Default)]
struct GateReport {
    correct: usize,
    total: usize,
    wrong_skips: usize,
    skips_caught: usize,
    skips_possible: usize,
}

impl GateReport {
    fn add(&mut self, gold: bool, pred: bool) {
        self.total += 1;
        if gold == pred {
            self.correct += 1;
        }
        if gold && !pred {
            self.wrong_skips += 1;
        }
        if !gold {
            self.skips_possible += 1;
            if !pred {
                self.skips_caught += 1;
            }
        }
    }

    fn print(&self, name: &str) {
        println!(
            "  {name:<28} accuracy {:>5.1}%  skips caught {:>2}/{}  wrong skips {:>2}/{}",
            pct(self.correct, self.total),
            self.skips_caught,
            self.skips_possible,
            self.wrong_skips,
            self.total - self.skips_possible
        );
    }
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let i = ((sorted_ms.len() - 1) as f64 * p).round() as usize;
    sorted_ms[i]
}

#[tokio::main]
async fn main() -> Result<()> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/data/prompt_intent_eval.json"
    );
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let items = serde_json::from_str::<EvalFile>(&raw)?.items;

    let load_start = Instant::now();
    let provider = OnnxProvider::new().context(
        "the default ONNX embedding model did not load; stage it and set ORT_DYLIB_PATH (CLAUDE.md)",
    )?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1e3;
    // Warm the session so the first timed call is not the cold one.
    provider.embed("warm up").await?;

    let build_start = Instant::now();
    let classifier = PrototypeClassifier::build(&provider, PrototypeParams::default()).await?;
    let build_ms = build_start.elapsed().as_secs_f64() * 1e3;

    // Embed every prompt once; every parameter setting below reuses them.
    let mut vectors = Vec::with_capacity(items.len());
    let mut embed_ms = Vec::with_capacity(items.len());
    for item in &items {
        let t = Instant::now();
        vectors.push(provider.embed(&item.prompt).await?);
        embed_ms.push(t.elapsed().as_secs_f64() * 1e3);
    }
    embed_ms.sort_by(f64::total_cmp);

    let t = Instant::now();
    for v in &vectors {
        std::hint::black_box(classifier.classify_vector(v));
    }
    let classify_us = t.elapsed().as_secs_f64() * 1e6 / vectors.len() as f64;

    println!("Prompt-intent evaluation: {} prompts", items.len());
    println!(
        "  model {}  ({} seeds, {} dims)",
        classifier.model_id(),
        classifier.seed_count(),
        provider.dimensions()
    );
    println!(
        "  latency: model load {load_ms:.0} ms, seed embedding {build_ms:.0} ms (once), \
         prompt embed p50 {:.1} ms / p95 {:.1} ms, classify {classify_us:.1} µs",
        percentile(&embed_ms, 0.5),
        percentile(&embed_ms, 0.95)
    );

    // --- 1. Situation.
    println!("\n1. Situation");
    let mut keyword = SituationReport::new();
    let mut proto = SituationReport::new();
    let mut proto_misses = Vec::new();
    for (item, v) in items.iter().zip(&vectors) {
        let gold = LABELS
            .into_iter()
            .find(|l| *l == item.situation)
            .with_context(|| format!("unknown situation label {:?}", item.situation))?;
        keyword.add(item, gold, label(keyword_situation(&item.prompt)));
        let d = classifier.classify_vector(v);
        // Score the situation on its own: a gate skip forces `None`, which
        // would count gate errors twice.
        let pred = if d.needs_context {
            label(d.situation)
        } else {
            "none"
        };
        proto.add(item, gold, pred);
        if pred != gold {
            proto_misses.push(format!("    {:<14} -> {:<14} {}", gold, pred, item.prompt));
        }
    }
    keyword.print("keyword (shipped)");
    proto.print("prototype (top_k=3)");

    for k in [1, 5] {
        let c = PrototypeClassifier::build(&provider, PrototypeParams::default())
            .await?
            .with_params(PrototypeParams {
                top_k: k,
                ..PrototypeParams::default()
            });
        let mut r = SituationReport::new();
        for (item, v) in items.iter().zip(&vectors) {
            let gold = LABELS.into_iter().find(|l| *l == item.situation).unwrap();
            let d = c.classify_vector(v);
            r.add(
                item,
                gold,
                if d.needs_context {
                    label(d.situation)
                } else {
                    "none"
                },
            );
        }
        println!(
            "  prototype (top_k={k})    accuracy {:>5.1}%  macro-F1 {:.3}",
            pct(r.correct, r.total),
            r.macro_f1()
        );
    }

    // --- 2. Context gate.
    println!("\n2. Context gate");
    let mut always = GateReport::default();
    let mut words = GateReport::default();
    for item in &items {
        always.add(item.needs_context, true);
        words.add(item.needs_context, word_list_needs_context(&item.prompt));
    }
    always.print("always search (shipped)");
    words.print("word-list rule");

    let mut gate_misses = Vec::new();
    for margin in [-0.05f32, 0.0, 0.05, 0.10, 0.15] {
        let c = PrototypeClassifier::build(&provider, PrototypeParams::default())
            .await?
            .with_params(PrototypeParams {
                gate_margin: margin,
                ..PrototypeParams::default()
            });
        let mut r = GateReport::default();
        for (item, v) in items.iter().zip(&vectors) {
            let d = c.classify_vector(v);
            r.add(item.needs_context, d.needs_context);
            if margin == PrototypeParams::default().gate_margin
                && d.needs_context != item.needs_context
            {
                gate_misses.push(format!(
                    "    gold {:<5} pred {:<5} (no_context {:.3} vs best task {:.3}) {}",
                    item.needs_context,
                    d.needs_context,
                    d.scores.no_context,
                    d.scores.debugging.max(d.scores.design).max(d.scores.other),
                    item.prompt
                ));
            }
        }
        let default_mark = if margin == PrototypeParams::default().gate_margin {
            " (default)"
        } else {
            ""
        };
        r.print(&format!("prototype margin {margin:+.2}{default_mark}"));
    }

    println!("\nPrototype situation errors (gold -> predicted):");
    for m in &proto_misses {
        println!("{m}");
    }
    println!("\nPrototype gate errors at the default margin:");
    for m in &gate_misses {
        println!("{m}");
    }
    Ok(())
}
