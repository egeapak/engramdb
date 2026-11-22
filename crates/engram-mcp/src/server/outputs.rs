//! The tools' result payloads and the helpers that build them.

use std::path::PathBuf;

use serde::Serialize;

use crate::error::{error_response, ErrorCode};
use engramdb::ops;

// ---------------------------------------------------------------------------
// Serialisable output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct CreateOutput {
    pub(super) id: String,
    pub(super) created: bool,
    pub(super) summary: String,
    /// `links` entries that could not be added (the memory itself was
    /// written): one message per failed edge.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) link_errors: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct MemoryOutput {
    id: String,
    #[serde(rename = "type")]
    type_: String,
    summary: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
    physical: Vec<String>,
    logical: Vec<String>,
    tags: Vec<String>,
    criticality: f64,
    confidence: f64,
    status: String,
    /// `shared` memories travel with the repo (`.engramdb/memories/` arrives
    /// via `git clone`), so their content is repo-authored; `personal` ones
    /// are the local user's. Surfaced so the consuming agent can weigh trust.
    visibility: String,
    /// Who recorded the memory: human|agent|inferred|imported.
    provenance: String,
    /// Epistemic class: fact|observation|decision (always present, §5.4).
    epistemic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_while: Option<engramdb::types::Validity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalidated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    /// When the memory was last re-confirmed via `verify` (facts decay from
    /// this anchor, so agents can see how stale a verification is).
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Defang every string value in `value`, recursively, with
/// [`ops::harvest::defang_memory_text`]. Keys are left alone.
///
/// One catch-all rather than a call per field: a memory's summary, content,
/// details, tags, scopes, edge notes and whatever field is added later are
/// all text that whoever wrote the memory file chose, and a field that is
/// forgotten is a field that reaches the agent as written. Ids, enum names
/// and timestamps pass through unchanged (they hold nothing to escape).
pub(super) fn defang_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let clean = ops::harvest::defang_memory_text(text);
            if clean != *text {
                *text = clean;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(defang_json),
        serde_json::Value::Object(map) => map.values_mut().for_each(defang_json),
        _ => {}
    }
}

/// Serialize a tool result that carries stored memory text: every string
/// defanged ([`defang_json`]) and `trust` as the **first** key.
///
/// `body` must be a JSON object; its keys keep their names. `trust` leads
/// because the wrapper is a struct: inside a `json!{}` object its place would
/// depend on serde_json's `preserve_order` feature (on since lancedb 0.41;
/// without it, `trust` sorted behind `total`). The harvest listings are
/// structs for the same reason.
pub(super) fn trusted_payload(mut body: serde_json::Value) -> Result<String, String> {
    #[derive(serde::Serialize)]
    struct Trusted {
        trust: &'static str,
        #[serde(flatten)]
        body: serde_json::Value,
    }
    defang_json(&mut body);
    serde_json::to_string(&Trusted {
        trust: ops::harvest::MEMORY_TRUST_HEADER,
        body,
    })
    .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
}

pub(super) fn memory_to_output(m: &engramdb::types::Memory, include_details: bool) -> MemoryOutput {
    MemoryOutput {
        id: m.id.clone(),
        type_: format!("{:?}", m.type_).to_lowercase(),
        summary: m.summary.clone(),
        content: m.content.clone(),
        details: if include_details {
            m.details.clone()
        } else {
            None
        },
        physical: m.physical.clone(),
        logical: m.logical.clone(),
        tags: m.tags.clone(),
        criticality: m.criticality,
        confidence: m.confidence,
        status: format!("{:?}", m.status).to_lowercase(),
        visibility: format!("{:?}", m.visibility).to_lowercase(),
        provenance: format!("{:?}", m.provenance.source).to_lowercase(),
        epistemic: m.epistemic.as_str().to_string(),
        valid_while: m.valid_while.clone(),
        valid_from: m.valid_from,
        invalidated_at: m.invalidated_at,
        superseded_by: m.superseded_by.clone(),
        verified_at: m.verified_at,
    }
}

#[derive(Serialize)]
pub(super) struct ScoreBreakdownOutput {
    pub(super) final_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) semantic: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) keyword: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) rerank: Option<f64>,
    pub(super) relevance: f64,
    pub(super) scope: f64,
    pub(super) scope_multiplier: f64,
    pub(super) trust: f64,
    pub(super) trust_multiplier: f64,
    pub(super) situation_multiplier: f64,
    pub(super) decay: f64,
    pub(super) criticality: f64,
    // Phase-5 graph/usage components: present only when their knob is on and
    // they changed the score, so a default config's output is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) contradicts_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) contradicted_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) supports_multiplier: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) supported_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) usage_multiplier: Option<f64>,
}

#[derive(Serialize)]
pub(super) struct ScoredMemoryOutput {
    #[serde(flatten)]
    pub(super) memory: MemoryOutput,
    pub(super) score: f64,
    /// Rank mode's fallback: a weak match returned because nothing cleared the
    /// relevance threshold. Present only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) below_threshold: bool,
    /// Set when the result was added by `expand_links`: the direct hit it was
    /// reached from and the edge kind followed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) via: Option<engramdb::scoring::LinkVia>,
    pub(super) score_breakdown: ScoreBreakdownOutput,
}

// ---------------------------------------------------------------------------
// Harvest listing payloads
// ---------------------------------------------------------------------------
//
// `struct`s rather than `serde_json::json!{}`, for the reason
// `ops::harvest::DigestJson` already documents: without serde_json's
// `preserve_order` feature a `json!{}` object serializes its keys
// alphabetically — which would bury `trust` in the middle of the payload it is
// supposed to introduce. A derived `Serialize` writes fields in declaration
// order whether the feature is on (it is, since lancedb 0.41) or off.

/// `harvest_list`.
#[derive(Serialize)]
pub(super) struct HarvestListJson<'a> {
    pub(super) trust: &'static str,
    pub(super) scope: &'a [PathBuf],
    pub(super) sessions: Vec<serde_json::Value>,
}

/// `harvest_search`.
#[derive(Serialize)]
pub(super) struct HarvestSearchJson {
    pub(super) trust: &'static str,
    pub(super) conversations: Vec<serde_json::Value>,
    pub(super) skipped_projects: Option<Vec<serde_json::Value>>,
    pub(super) hint: &'static str,
}

/// `harvest_ledger`.
///
/// An object around what used to be a bare array, so the marker has somewhere
/// to live. The rows are unchanged and now sit under `entries`.
#[derive(Serialize)]
pub(super) struct HarvestLedgerJson {
    pub(super) trust: &'static str,
    pub(super) entries: Vec<serde_json::Value>,
}

/// One `harvest_search` hit.
///
/// A free function rather than a closure inside the tool so it can be asserted
/// on without an embedding model: what it guards is a property of the payload,
/// not of the search.
///
/// Every string here is recorded content and goes through the digest's own
/// pipeline — sanitize, bound, defang harness tags — rather than the sanitizer
/// alone. `ops::harvest_index` now cleans these fields on the way *into* the
/// table too, but rows written before it did are still on disk, and a
/// `sanitize_one_line` never touched a harness tag in the first place.
pub(super) fn harvest_hit_json(h: &engramdb::storage::ConversationHit) -> serde_json::Value {
    let clean =
        |v: &Option<String>| -> Option<String> { v.as_deref().map(ops::harvest::defang_metadata) };
    serde_json::json!({
        "session_id": ops::harvest::defang_metadata(&h.session_id),
        "project_id": h.project_id,
        "score": (h.score * 1000.0).round() / 1000.0,
        "matched_on": match h.matched_on {
            engramdb::storage::MatchedOn::Digest => "digest",
            engramdb::storage::MatchedOn::Summary => "summary",
        },
        "cwd": clean(&h.cwd),
        "git_branch": clean(&h.git_branch),
        "started_at": h.started_at,
        "ended_at": h.ended_at,
        "first_prompt": clean(&h.first_prompt),
        "summary": h.summary.as_deref().map(ops::harvest::defang_prose),
        // A partial row is a session whose tail was never embedded, so a miss
        // against it is not evidence the topic was absent.
        "indexed_complete": h.indexed_complete,
    })
}
