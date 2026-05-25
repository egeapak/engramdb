//! Quick label sanity for int8 vs fp32 NLI (Lever D default flip).
//!
//! int8 NLI is now the production default ([`DEFAULT_NLI_MODEL`]). Speed is
//! moot if it misclassifies, so this runs labeled premise/hypothesis pairs
//! through both fp32 ([`NLI_DEBERTA_XSMALL`]) and int8
//! ([`NLI_DEBERTA_XSMALL_Q`]) and checks (a) int8 matches the expected
//! label and (b) int8 agrees with fp32.
//!
//! Run: `cargo run --release --example nli_quality`

use anyhow::Result;
use engramdb::nli::{
    NliLabel, NliProvider, OnnxNliProvider, NLI_DEBERTA_XSMALL, NLI_DEBERTA_XSMALL_Q,
};
use engramdb::onnx_ep::Backend;

/// (premise, hypothesis, expected label) — the three NLI classes, with
/// EngramDB-flavored contradiction cases (the use case that matters: a new
/// memory contradicting an existing one).
const CASES: &[(&str, &str, NliLabel)] = &[
    (
        "The database uses PostgreSQL.",
        "The database uses MySQL.",
        NliLabel::Contradiction,
    ),
    (
        "We cache the embedding provider behind an Arc.",
        "The embedding provider is created fresh on every call.",
        NliLabel::Contradiction,
    ),
    (
        "The retrieval hook must complete in under 50ms.",
        "The retrieval hook has a strict latency budget.",
        NliLabel::Entailment,
    ),
    (
        "All model downloads cache under the engramdb cache dir.",
        "Models are cached so they download once per machine.",
        NliLabel::Entailment,
    ),
    (
        "The project uses LanceDB for vector storage.",
        "The team plans to add a dark mode to the UI.",
        NliLabel::Neutral,
    ),
];

#[tokio::main]
async fn main() -> Result<()> {
    let fp32 = OnnxNliProvider::with_spec_on(&NLI_DEBERTA_XSMALL, Backend::Cpu)?;
    let int8 = OnnxNliProvider::with_spec_on(&NLI_DEBERTA_XSMALL_Q, Backend::Cpu)?;

    let mut fp32_ok = 0;
    let mut int8_ok = 0;
    let mut agree = 0;

    for (premise, hypothesis, expected) in CASES {
        let f = fp32.classify(premise, hypothesis).await?.label;
        let i = int8.classify(premise, hypothesis).await?.label;
        fp32_ok += usize::from(f == *expected);
        int8_ok += usize::from(i == *expected);
        agree += usize::from(f == i);
        println!("  expect {expected:<14?} fp32 {f:<14?} int8 {i:<14?} p={premise:?}");
    }

    let n = CASES.len();
    println!("\nint8 vs fp32 NLI sanity ({n} cases):");
    println!("  correct label   fp32 {fp32_ok}/{n}   int8 {int8_ok}/{n}");
    println!("  fp32==int8 agreement: {agree}/{n}");
    Ok(())
}
