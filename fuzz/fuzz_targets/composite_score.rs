#![no_main]

use arbitrary::Arbitrary;
use chrono::{DateTime, Utc};
use libfuzzer_sys::fuzz_target;

use engramdb::scoring::{composite_score, ScoringContext};
use engramdb::types::{
    ChallengePenalty, EngramConfig, Epistemic, Memory, MemoryType, Provenance, ProvenanceSource,
    Situation, Status,
};

/// The fuzzed inputs, as a named struct rather than a tuple.
///
/// This deliberately is *not* a tuple: std implements `Debug` only up to arity
/// 12, and `fuzz_target!` formats the input with `{:?}` to print failing cases.
/// This target started at 11 fields and silently stopped building when #60
/// added the epistemic ones (#64). A struct has no such ceiling, so new fields
/// can be added freely.
#[derive(Debug, Arbitrary)]
struct ScoreInput {
    type_sel: u8,
    criticality: f64,
    confidence: f64,
    created_ts: i64,
    physical: Vec<String>,
    logical: Vec<String>,
    path: Option<String>,
    ctx_logical: Vec<String>,
    keyword_score: Option<f64>,
    semantic_score: Option<f64>,
    now_ts: i64,
    epistemic_sel: u8,
    situation_sel: u8,
    verified_ts: Option<i64>,
    /// Per-class challenge penalties: (fact, observation, decision).
    penalties: (f64, f64, f64),
}

// `composite_score` is the heart of ranking: it folds criticality, decay,
// scope, trust, the situation multiplier and an optional semantic/keyword
// signal into a single `final_score` that retrieval sorts on. Several of its
// inputs originate from untrusted on-disk memory files — notably
// `criticality`, which is parsed with a plain `f64::parse` (so a file can
// carry `NaN`/`inf`), plus the scope vectors, timestamps, the epistemic class
// and `verified_at` (the fact freshness anchor). Per-class challenge-penalty
// config values are fuzzed too (the fuzzer ignores config validation). A
// non-finite `final_score` is dangerous: ranking sorts with
// `partial_cmp(..).unwrap()`, which panics on NaN. This target asserts the
// score is always finite no matter how hostile the memory is.
fuzz_target!(|input: ScoreInput| {
    let ScoreInput {
        type_sel,
        criticality,
        confidence,
        created_ts,
        physical,
        logical,
        path,
        ctx_logical,
        keyword_score,
        semantic_score,
        now_ts,
        epistemic_sel,
        situation_sel,
        verified_ts,
        penalties,
    } = input;

    let (Some(created_at), Some(now)) = (
        DateTime::<Utc>::from_timestamp(created_ts, 0),
        DateTime::<Utc>::from_timestamp(now_ts, 0),
    ) else {
        return;
    };

    let type_ = match type_sel % 8 {
        0 => MemoryType::Decision,
        1 => MemoryType::Convention,
        2 => MemoryType::Hazard,
        3 => MemoryType::Context,
        4 => MemoryType::Intent,
        5 => MemoryType::Relationship,
        6 => MemoryType::Debug,
        _ => MemoryType::Preference,
    };

    let mut memory = Memory::new(type_, "", "", Provenance::new(ProvenanceSource::Agent));
    memory.criticality = criticality;
    memory.confidence = confidence;
    memory.created_at = created_at;
    memory.physical = physical;
    memory.logical = logical;
    memory.epistemic = match epistemic_sel % 3 {
        0 => Epistemic::Fact,
        1 => Epistemic::Observation,
        _ => Epistemic::Decision,
    };
    // Fact freshness anchor: arbitrary (possibly far-future) verified_at.
    memory.verified_at = verified_ts.and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0));
    // Exercise the challenge-penalty branch as well.
    if type_sel & 0x80 != 0 {
        memory.status = Status::Challenged;
    }

    let situation = match situation_sel % 5 {
        0 => None,
        1 => Some(Situation::SessionStart),
        2 => Some(Situation::FileEdit),
        3 => Some(Situation::Debugging),
        _ => Some(Situation::DesignChoice),
    };

    let context = ScoringContext {
        path: path.as_deref(),
        logical: &ctx_logical,
        query: path.as_deref(),
        keyword_score,
        semantic_score,
        embeddings_available: semantic_score.is_some(),
        situation,
    };

    let mut config = EngramConfig::default();
    // Per-class penalties straight from the fuzzer (NaN/inf included) — the
    // formula's read-side clamp, not config validation, is what must hold.
    let (fact, observation, decision) = penalties;
    config.retrieval.scoring.challenge_penalty = if situation_sel & 0x80 != 0 {
        ChallengePenalty::Flat(fact)
    } else {
        ChallengePenalty::PerClass {
            fact,
            observation,
            decision,
        }
    };

    let breakdown = composite_score(&memory, &context, &config, now);
    assert!(
        breakdown.final_score.is_finite(),
        "composite_score produced a non-finite final_score (criticality={criticality})"
    );
});
