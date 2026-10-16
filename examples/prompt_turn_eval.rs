//! Evaluate the context gate with the agent's previous message and an NLI
//! "does this ask for work?" check.
//!
//! `examples/prompt_intent_eval.rs` judges a prompt on its own text. Short
//! replies such as "push it", "yes" or "ok" cannot be judged that way: "yes"
//! after "Should I push?" asks for work, and "yes" after "Done, it is pushed"
//! does not. This example measures two additions:
//!
//! - **NLI check.** For short prompts, ask the NLI model whether the premise
//!   entails a hypothesis such as "The user asks the assistant to do
//!   something." The premise is the prompt alone, or the prompt with the
//!   agent's previous message.
//! - **Previous message.** The UserPromptSubmit hook event carries
//!   `transcript_path`, so the hook can read the agent's last message.
//!
//! Data: `examples/data/prompt_turn_eval.json` (60 hand-labelled pairs).
//! Gold `search` means the turn asks for work, including approving or
//! choosing work the agent proposed.
//!
//! Run: `cargo run --release --example prompt_turn_eval`
//! (needs the default ONNX embedding and NLI models in the model cache and an
//! ONNX Runtime shared library; see CLAUDE.md).

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use engramdb::embeddings::OnnxProvider;
use engramdb::nli::{NliProvider, OnnxNliProvider, DEFAULT_NLI_MODEL};
use engramdb::onnx_ep::Backend;
use engramdb::prompt_intent::{PrototypeClassifier, PrototypeParams};
use serde::Deserialize;

#[derive(Deserialize)]
struct EvalFile {
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    last: String,
    prompt: String,
    search: bool,
    #[serde(default)]
    tags: Vec<String>,
}

/// Hypotheses tried, fixed before the first run.
const HYPOTHESES: &[&str] = &[
    "The user asks the assistant to do something.",
    "The user wants the assistant to continue working.",
    "The user is asking for an action.",
];

/// Hypotheses about the agent's previous message alone: did it ask the user
/// for something? Added after the dialogue premises above failed.
const ASK_HYPOTHESES: &[&str] = &[
    "The assistant asks the user a question.",
    "The assistant is waiting for the user's approval.",
];

/// Entailment-probability cut-offs tried for each hypothesis.
const THRESHOLDS: &[f32] = &[0.3, 0.5, 0.7];

/// Prompts with at most this many words go to the NLI check; longer ones
/// keep the prototype's decision. Matches `PrototypeParams::max_skip_words`.
const NLI_MAX_WORDS: usize = 7;

fn prompt_only_premise(prompt: &str) -> String {
    format!("The user said: \"{prompt}\"")
}

fn context_premise(last: &str, prompt: &str) -> String {
    format!("The assistant said: \"{last}\" The user replied: \"{prompt}\"")
}

#[derive(Default)]
struct GateReport {
    correct: usize,
    total: usize,
    wrong_skips: usize,
    skips_caught: usize,
    skips_possible: usize,
    by_tag: BTreeMap<String, (usize, usize)>,
}

impl GateReport {
    fn add(&mut self, item: &Item, pred: bool) {
        let gold = item.search;
        let hit = gold == pred;
        self.total += 1;
        self.correct += usize::from(hit);
        self.wrong_skips += usize::from(gold && !pred);
        if !gold {
            self.skips_possible += 1;
            self.skips_caught += usize::from(!pred);
        }
        for tag in &item.tags {
            let e = self.by_tag.entry(tag.clone()).or_default();
            e.1 += 1;
            e.0 += usize::from(hit);
        }
    }

    fn print(&self, name: &str) {
        let tags: Vec<String> = self
            .by_tag
            .iter()
            .map(|(t, (c, n))| format!("{t} {c}/{n}"))
            .collect();
        println!(
            "  {name:<58} acc {:>5.1}%  skips {:>2}/{}  wrong skips {:>2}/{}  [{}]",
            100.0 * self.correct as f64 / self.total as f64,
            self.skips_caught,
            self.skips_possible,
            self.wrong_skips,
            self.total - self.skips_possible,
            tags.join(", ")
        );
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/data/prompt_turn_eval.json"
    );
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let items = serde_json::from_str::<EvalFile>(&raw)?.items;

    let embedder = OnnxProvider::new().context("default ONNX embedding model did not load")?;
    let nli = OnnxNliProvider::with_spec_on(&DEFAULT_NLI_MODEL, Backend::Cpu)
        .context("default NLI model did not load")?;
    let classifier = PrototypeClassifier::build(&embedder, PrototypeParams::default()).await?;

    // Prototype decision per item (prompt text only).
    let mut proto = Vec::with_capacity(items.len());
    for item in &items {
        proto.push(
            classifier
                .classify(&embedder, &item.prompt)
                .await?
                .needs_context,
        );
    }

    // Entailment per (item, hypothesis), for both premise forms. Timed per
    // pair so the hook cost is visible.
    nli.classify("warm up", "warm up").await?;
    let mut alone = vec![vec![0f32; HYPOTHESES.len()]; items.len()];
    let mut with_ctx = vec![vec![0f32; HYPOTHESES.len()]; items.len()];
    let mut pair_ms = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let p_alone = prompt_only_premise(&item.prompt);
        let p_ctx = context_premise(&item.last, &item.prompt);
        for (h, hyp) in HYPOTHESES.iter().enumerate() {
            alone[i][h] = nli.classify(&p_alone, hyp).await?.entailment;
            let t = Instant::now();
            with_ctx[i][h] = nli.classify(&p_ctx, hyp).await?.entailment;
            pair_ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
    }
    pair_ms.sort_by(f64::total_cmp);

    let short = |item: &Item| item.prompt.split_whitespace().count() <= NLI_MAX_WORDS;

    println!(
        "Context-gate evaluation: {} pairs ({} search, {} skip)",
        items.len(),
        items.iter().filter(|i| i.search).count(),
        items.iter().filter(|i| !i.search).count()
    );
    println!(
        "  NLI {} with previous message: p50 {:.1} ms, p95 {:.1} ms per pair",
        DEFAULT_NLI_MODEL.repo,
        pair_ms[pair_ms.len() / 2],
        pair_ms[(pair_ms.len() - 1) * 95 / 100]
    );

    println!("\nBaselines");
    let mut r = GateReport::default();
    for item in &items {
        r.add(item, true);
    }
    r.print("always search (shipped)");
    let mut r = GateReport::default();
    for (item, p) in items.iter().zip(&proto) {
        r.add(item, *p);
    }
    r.print("prototype gate, prompt only");

    for (title, scores) in [
        ("NLI on the prompt alone", &alone),
        ("NLI on the prompt with the previous message", &with_ctx),
    ] {
        println!(
            "\n{title} (prompts of {NLI_MAX_WORDS} words or fewer; longer keep the prototype)"
        );
        for (h, hyp) in HYPOTHESES.iter().enumerate() {
            println!("  hypothesis: {hyp}");
            for &t in THRESHOLDS {
                let mut r = GateReport::default();
                for (i, item) in items.iter().enumerate() {
                    let pred = if short(item) {
                        scores[i][h] >= t
                    } else {
                        proto[i]
                    };
                    r.add(item, pred);
                }
                r.print(&format!("    entailment >= {t:.1}"));
            }
        }
    }

    // Split decision: the prototype judges the prompt text, NLI judges only
    // whether the previous message asked the user for something. Search when
    // the prototype finds a task, or when the agent was waiting for a reply.
    let mut asks = vec![vec![0f32; ASK_HYPOTHESES.len()]; items.len()];
    for (i, item) in items.iter().enumerate() {
        for (h, hyp) in ASK_HYPOTHESES.iter().enumerate() {
            asks[i][h] = nli.classify(&item.last, hyp).await?.entailment;
        }
    }
    println!("\nPrototype on the prompt, plus NLI on the previous message alone");
    println!("  (search when the prototype finds a task, or the previous message asked the user)");
    for (h, hyp) in ASK_HYPOTHESES.iter().enumerate() {
        println!("  hypothesis: {hyp}");
        for &t in THRESHOLDS {
            let mut r = GateReport::default();
            for (i, item) in items.iter().enumerate() {
                r.add(item, proto[i] || asks[i][h] >= t);
            }
            r.print(&format!("    entailment >= {t:.1}"));
        }
    }

    // Per-item view for the context premise with the first hypothesis.
    println!(
        "\nPer item: hypothesis 1 alone / with previous message, \
         previous message asks (ask hypothesis 1), prototype decision"
    );
    for (i, item) in items.iter().enumerate() {
        println!(
            "  gold {:<6} {:.2} / {:.2}  asks {:.2}  proto {:<6} {:<34} | {}",
            if item.search { "search" } else { "skip" },
            alone[i][0],
            with_ctx[i][0],
            asks[i][0],
            if proto[i] { "search" } else { "skip" },
            item.prompt,
            item.last
        );
    }
    Ok(())
}
