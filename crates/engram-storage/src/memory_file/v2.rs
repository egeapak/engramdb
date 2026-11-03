//! V2 format: minimal YAML frontmatter + structured markdown sections.
//!
//! ```text
//! ---
//! version: 2
//! id: <uuid>
//! type: <memory_type>
//! status: <status>
//! ---
//!
//! # <summary>
//!
//! ## Content
//! <content text>
//!
//! ## Details
//! <optional details>
//!
//! ## Scope
//! - **Files:** `src/db/**`
//! - **Tags:** database, transactions
//! - **Criticality:** 0.95
//! - **Confidence:** 1.0
//!
//! ## Provenance
//! - **Source:** human
//! - **Created:** 2026-01-15T10:00:00Z
//!
//! <!-- engramdb
//! visibility: shared
//! accessed_at: "2026-01-15T10:00:00Z"
//! -->
//! ```
//!
//! # Content escaping
//!
//! The free-text sections (`## Content`, `## Details`) may themselves contain
//! markdown that looks like this format's structure. To keep files round-trip
//! safe, the writer prefixes any structural-looking content line (`# `, `## `,
//! `<!--`, `-->`, `> `) with a single backslash, and the parser strips it back
//! off; a content line that already carries such an escape gains one more
//! backslash (`\## x` is written as `\\## x`). The hidden `<!-- engramdb -->`
//! block is only recognized as the *last* line-anchored marker in the file —
//! where the writer puts it. Files written before escaping existed still parse
//! (they contain no escapes); see `helpers` module docs for details.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::helpers::{
    escape_body_text, format_provenance_source, parse_body_sections, parse_datetime_field,
    parse_list_field, parse_provenance_section, parse_score_field, sanitize_list_item,
    sanitize_single_line, split_frontmatter, HIDDEN_META_END, HIDDEN_META_START,
};
use super::{yaml, MemoryParser, MemoryWriter, CURRENT_FORMAT_VERSION};
use crate::error::{Result, StorageError};
use engram_types::{
    Challenge, Decay, Epistemic, Link, Memory, MemoryType, Status, Validity, Visibility,
};

// ---------------------------------------------------------------------------
// Frontmatter structs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct MinimalFrontmatter {
    version: u32,
    id: String,
    #[serde(rename = "type")]
    type_: MemoryType,
    status: Status,
    /// Epistemic class. Frontmatter (not hidden meta) because it is
    /// human-meaningful and human-editable, like `type`/`status`. Written
    /// only when off-diagonal (`!= type_.default_epistemic()`), so files
    /// carrying the type-derived default round-trip byte-identically to
    /// files written before the field existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    epistemic: Option<Epistemic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct HiddenMeta {
    visibility: Option<Visibility>,
    #[serde(skip_serializing_if = "Option::is_none")]
    accessed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_while: Option<Validity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_from: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalidated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decay: Option<Decay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supersedes: Vec<String>,
    /// Multi-project audience (per-memory sharing precision). `None` ⇒ visible
    /// to the whole store's group. Machine metadata, so it lives in the hidden
    /// block rather than the human-facing frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audience: Option<Vec<String>>,
    /// Sessions this memory was extracted from (schema v0.7.0). In the hidden
    /// block rather than the `## Provenance` section because it is a machine
    /// relation — it is what exempts a transcript copy from eviction — and
    /// because the provenance section is parsed as single-valued `**Field:**`
    /// lines, which cannot carry a list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_sessions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    challenges: Vec<Challenge>,
    /// Outbound typed edges (schema v0.9.0). Last, so a file without edges
    /// is byte-identical to one written before the field existed, and a file
    /// with them only grows at the end of the block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<Link>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parser for the V2 structured markdown format.
pub struct V2Parser;

impl MemoryParser for V2Parser {
    fn parse(&self, content: &str) -> Result<Memory> {
        let (frontmatter, body) = split_frontmatter(content)?;
        parse_v2(frontmatter, body)
    }

    fn version(&self) -> Option<u32> {
        Some(CURRENT_FORMAT_VERSION)
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Writer that produces the V2 structured markdown format.
pub struct V2Writer;

impl MemoryWriter for V2Writer {
    fn write(&self, memory: &Memory) -> Result<String> {
        write_v2(memory)
    }

    fn version(&self) -> Option<u32> {
        Some(CURRENT_FORMAT_VERSION)
    }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

fn parse_v2(frontmatter: &str, body: &str) -> Result<Memory> {
    let fm: MinimalFrontmatter = yaml::from_str(frontmatter)?;
    super::validate_id_shape(&fm.id)?;

    let sections = parse_body_sections(body);

    let h1 = sections.get("__h1__").cloned().unwrap_or_default();
    // When title is set, H1 is the title and summary is in a **Summary:** line.
    // Never fall back to the H1 here: for a titled memory the H1 *is* the
    // title, so a memory whose summary is legitimately empty would acquire the
    // title on re-parse and the write would stop being a fixed point (#62).
    let summary = if fm.title.is_some() {
        extract_bold_field(body, "Summary").unwrap_or_default()
    } else {
        h1
    };
    let content = sections.get("Content").cloned().unwrap_or_default();
    let details = sections.get("Details").cloned().filter(|s| !s.is_empty());

    // Parse scope
    let scope_text = sections.get("Scope").map(String::as_str).unwrap_or("");
    let physical = parse_list_field(scope_text, "Files");
    let logical = parse_list_field(scope_text, "Logical");
    let tags = parse_list_field(scope_text, "Tags");
    let criticality = parse_score_field(scope_text, "Criticality").unwrap_or(0.5);
    let confidence = parse_score_field(scope_text, "Confidence").unwrap_or(0.8);
    let visibility_from_scope = visibility_from_scope_text(scope_text);

    // Parse provenance
    let prov_text = sections.get("Provenance").map(String::as_str).unwrap_or("");
    let provenance = parse_provenance_section(prov_text);
    let created_at = parse_datetime_field(prov_text, "Created").unwrap_or_else(Utc::now);
    let updated_at = parse_datetime_field(prov_text, "Updated").unwrap_or(created_at);

    // Parse hidden metadata
    let hidden = parse_hidden_meta(body)?;

    Ok(Memory {
        id: fm.id,
        type_: fm.type_,
        epistemic: fm.epistemic.unwrap_or_else(|| fm.type_.default_epistemic()),
        // An all-empty Validity is meaningless; normalize it away on read so
        // hand-edited files can't smuggle one into the domain model.
        valid_while: hidden.valid_while.filter(|v| !v.is_empty()),
        valid_from: hidden.valid_from,
        invalidated_at: hidden.invalidated_at,
        superseded_by: hidden.superseded_by,
        title: fm.title,
        summary,
        content,
        details,
        physical,
        logical,
        tags,
        criticality,
        confidence,
        decay: hidden.decay,
        provenance,
        supersedes: hidden.supersedes,
        status: fm.status,
        visibility: hidden.visibility.unwrap_or(visibility_from_scope),
        // Normalize an empty audience away on read so a hand-edited file can't
        // smuggle a "visible to nobody" list into the domain model.
        audience: hidden.audience.filter(|a| !a.is_empty()),
        // Dropped rather than trusted when it is not a plain session id: this
        // value is later joined into a transcript path by the pin lookup, and
        // a memory file is hand-editable and travels with a clone.
        source_sessions: hidden
            .source_sessions
            .into_iter()
            .filter(|s| crate::transcripts::is_valid_session_id(s))
            .collect(),
        challenges: hidden.challenges,
        links: hidden.links,
        verified_at: hidden.verified_at,
        created_at,
        updated_at,
        accessed_at: hidden.accessed_at.unwrap_or(updated_at),
        expires_at: hidden.expires_at,
    })
}

fn write_v2(memory: &Memory) -> Result<String> {
    let mut out = String::new();

    // -- Minimal YAML frontmatter --
    out.push_str("---\n");
    let fm = MinimalFrontmatter {
        version: CURRENT_FORMAT_VERSION,
        id: memory.id.clone(),
        type_: memory.type_,
        status: memory.status,
        epistemic: (memory.epistemic != memory.type_.default_epistemic())
            .then_some(memory.epistemic),
        title: memory.title.clone(),
    };
    let yaml = yaml::to_string(&fm)?;
    out.push_str(&yaml);
    out.push_str("---\n\n");

    // -- H1: title (if set) or summary --
    //
    // Heading and summary are single-line constructs: collapse any embedded
    // newlines (see `sanitize_single_line`) so a hostile/malformed value
    // can't inject structural lines (`## Content`, `<!-- engramdb`) into the
    // body and shadow the real sections on re-parse.
    let heading = sanitize_single_line(memory.title.as_deref().unwrap_or(&memory.summary));
    out.push_str(&format!("# {heading}\n\n"));

    // When title is used as H1, write summary separately so it round-trips
    if memory.title.is_some() {
        out.push_str(&format!(
            "**Summary:** {}\n\n",
            sanitize_single_line(&memory.summary)
        ));
    }

    // -- ## Content --
    out.push_str("## Content\n\n");
    out.push_str(&escape_body_text(&memory.content));
    out.push('\n');

    // -- ## Details (optional) --
    if let Some(details) = &memory.details {
        out.push_str("\n## Details\n\n");
        out.push_str(&escape_body_text(details));
        out.push('\n');
    }

    // -- ## Scope --
    //
    // List items are backtick-delimited on a single structural line, so each
    // item is passed through `sanitize_list_item` (newlines collapsed,
    // delimiter backticks stripped) and empties are dropped — otherwise a
    // pathological item would desynchronize write→parse→write.
    let list_items = |items: &[String]| -> Vec<String> {
        items
            .iter()
            .map(|i| sanitize_list_item(i))
            .filter(|i| !i.is_empty())
            .map(|i| format!("`{i}`"))
            .collect()
    };
    out.push_str("\n## Scope\n\n");
    // Note `list_items` also decides line presence: a list whose every item
    // sanitizes away writes no line at all, matching what a re-parse yields.
    let files = list_items(&memory.physical);
    if !files.is_empty() {
        out.push_str(&format!("- **Files:** {}\n", files.join(", ")));
    }
    let logical = list_items(&memory.logical);
    if !logical.is_empty() {
        out.push_str(&format!("- **Logical:** {}\n", logical.join(", ")));
    }
    // Tags are backtick-wrapped like Files/Logical so a tag containing a
    // comma survives the round trip (the parser splits plain values on
    // commas).
    let tags = list_items(&memory.tags);
    if !tags.is_empty() {
        out.push_str(&format!("- **Tags:** {}\n", tags.join(", ")));
    }
    out.push_str(&format!("- **Criticality:** {}\n", memory.criticality));
    out.push_str(&format!("- **Confidence:** {}\n", memory.confidence));
    if memory.visibility == Visibility::Personal {
        out.push_str("- **Visibility:** personal\n");
    }

    // -- ## Provenance --
    out.push_str("\n## Provenance\n\n");
    out.push_str(&format!(
        "- **Source:** {}\n",
        format_provenance_source(memory.provenance.source)
    ));
    // Provenance values are caller-supplied free text on single-line fields:
    // collapse newlines so they can't inject structural lines (and so a
    // multi-line Reason can't smuggle a `- **Created:** …` line that would
    // hijack the timestamps on re-parse).
    if let Some(ref agent_id) = memory.provenance.agent_id {
        out.push_str(&format!(
            "- **Agent:** {}\n",
            sanitize_single_line(agent_id)
        ));
    }
    if let Some(ref model) = memory.provenance.model {
        out.push_str(&format!("- **Model:** {}\n", sanitize_single_line(model)));
    }
    if let Some(ref session_id) = memory.provenance.session_id {
        out.push_str(&format!(
            "- **Session:** {}\n",
            sanitize_single_line(session_id)
        ));
    }
    if let Some(ref reason) = memory.provenance.reason {
        out.push_str(&format!("- **Reason:** {}\n", sanitize_single_line(reason)));
    }
    out.push_str(&format!(
        "- **Created:** {}\n",
        memory.created_at.to_rfc3339()
    ));
    out.push_str(&format!(
        "- **Updated:** {}\n",
        memory.updated_at.to_rfc3339()
    ));

    // -- Hidden metadata (HTML comment with YAML) --
    let hidden = HiddenMeta {
        visibility: Some(memory.visibility),
        accessed_at: Some(memory.accessed_at),
        verified_at: memory.verified_at,
        expires_at: memory.expires_at,
        valid_while: memory.valid_while.clone().filter(|v| !v.is_empty()),
        valid_from: memory.valid_from,
        invalidated_at: memory.invalidated_at,
        superseded_by: memory.superseded_by.clone(),
        decay: memory.decay.clone(),
        supersedes: memory.supersedes.clone(),
        audience: memory.audience.clone().filter(|a| !a.is_empty()),
        source_sessions: memory.source_sessions.clone(),
        challenges: memory.challenges.clone(),
        links: memory.links.clone(),
    };
    let hidden_yaml = yaml::to_string(&hidden)?;
    out.push_str(&format!("\n<!-- engramdb\n{hidden_yaml}-->\n"));

    Ok(out)
}

/// Extract a value from a `**Field:** value` line in the body preamble.
///
/// Only the region before the first `## ` section heading is scanned — the
/// writer emits these lines right after the H1, so a `**Field:**` line inside
/// content can never hijack the value.
/// Determine visibility from the `## Scope` section text.
///
/// Reads the value of the `- **Visibility:** <value>` field on its own line,
/// rather than a substring `contains` over the whole section. A bare substring
/// match (the pre-fix behaviour) would flip a memory to Personal if the literal
/// text "**Visibility:** personal" appeared anywhere in the scope — e.g. inside
/// another field's value — even when the actual Visibility field said Shared
/// (finding #6).
fn visibility_from_scope_text(scope_text: &str) -> Visibility {
    for line in scope_text.lines() {
        // Match a `- **Visibility:** <value>` field line (leading `- ` and
        // surrounding whitespace tolerated), then read its value exactly.
        let trimmed = line.trim().trim_start_matches('-').trim();
        if let Some(rest) = trimmed.strip_prefix("**Visibility:**") {
            return if rest.trim().eq_ignore_ascii_case("personal") {
                Visibility::Personal
            } else {
                Visibility::Shared
            };
        }
    }
    Visibility::Shared
}

fn extract_bold_field(body: &str, field: &str) -> Option<String> {
    let prefix = format!("**{field}:** ");
    for line in body.lines() {
        if line.starts_with("## ") {
            break;
        }
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Extract the `<!-- engramdb ... -->` block and parse it as YAML.
///
/// The writer emits the block at the very end of the file with the markers on
/// their own lines, so the *last* line-anchored `<!-- engramdb` marker is the
/// authoritative one (a content-embedded block in an old unescaped file can
/// no longer hijack the metadata; new writes escape such lines anyway). For
/// legacy/hand-edited files without a line-anchored marker, fall back to the
/// historical first-substring scan.
///
/// # A present-but-broken block is an error, never a reset
///
/// This used to fall through to `HiddenMeta::default()` with a warning. The
/// block holds `challenges`, `supersedes`, `superseded_by`, `source_sessions`,
/// `links`, decay, expiry and validity, and every one of them is only ever
/// *read back from this block* — so a default here was not a degraded read,
/// it was the first half of a silent erase: the next `update` of any field
/// rewrote the file from the defaults, and a merge-conflict marker in one
/// field cost every other one. Now the parse fails, which makes the failure
/// loud on every surface instead of on none: `get`/`update` return the error
/// (so nothing can rewrite the file), `reindex` skips the file with a warning
/// naming it, and `doctor` lists it with this message.
///
/// Three shapes are refused: an opening marker with no closing `-->`, a
/// git conflict marker anywhere after the writer's last section (where no
/// writer-emitted line can start with one), and YAML that does not parse.
/// The legacy substring fallback stays lenient — it exists for files whose
/// content merely *mentions* the marker, where an error would make an
/// ordinary old file unreadable.
fn parse_hidden_meta(body: &str) -> Result<HiddenMeta> {
    let lines: Vec<&str> = body.lines().collect();

    // Conflict markers in the trailer. Everything after the last
    // `## Provenance` heading is writer-emitted single-line fields plus the
    // hidden block, so a line opening with a conflict marker there cannot be
    // content. Checked before the block is located: a conflict *around* two
    // whole blocks parses cleanly and would otherwise silently pick one side.
    // Without a provenance section (a hand-written file), only the block
    // itself is checked: a setext heading underline (`=======`) in content is
    // ordinary markdown.
    let trailer_start = lines
        .iter()
        .rposition(|l| l.trim_end() == "## Provenance")
        .or_else(|| {
            lines
                .iter()
                .rposition(|l| l.trim_end() == HIDDEN_META_START)
        })
        .unwrap_or(lines.len());
    if let Some(marker) = lines[trailer_start..]
        .iter()
        .find(|l| is_conflict_marker(l))
    {
        return Err(StorageError::InvalidFormat(format!(
            "unresolved merge conflict in the engramdb metadata block (line {:?}); \
             resolve it in the file, then run `engramdb reindex`",
            marker.trim_end()
        )));
    }

    if let Some(start) = lines
        .iter()
        .rposition(|line| line.trim_end() == HIDDEN_META_START)
    {
        let Some(len) = lines[start + 1..]
            .iter()
            .position(|line| line.trim_end() == HIDDEN_META_END)
        else {
            return Err(StorageError::InvalidFormat(
                "the engramdb metadata block (`<!-- engramdb`) is never closed by `-->`; \
                 fix the file by hand, then run `engramdb reindex`"
                    .to_string(),
            ));
        };
        let yaml_content = lines[start + 1..start + 1 + len].join("\n");
        let yaml_content = yaml_content.trim();
        if yaml_content.is_empty() {
            return Ok(HiddenMeta::default());
        }
        return yaml::from_str(yaml_content).map_err(|e| {
            StorageError::InvalidFormat(format!(
                "unparseable engramdb metadata block ({e}); nothing was reset — fix the \
                 file by hand, then run `engramdb reindex`"
            ))
        });
    }

    // Legacy fallback: first substring occurrence anywhere in the body.
    if let Some(start_idx) = body.find(HIDDEN_META_START) {
        let after_marker = &body[start_idx + HIDDEN_META_START.len()..];
        if let Some(end_idx) = after_marker.find(HIDDEN_META_END) {
            let yaml_content = after_marker[..end_idx].trim();
            if let Ok(meta) = yaml::from_str(yaml_content) {
                return Ok(meta);
            }
        }
    }

    Ok(HiddenMeta::default())
}

/// A git merge-conflict marker line (`<<<<<<< `, `=======`, `>>>>>>> `, and
/// diff3's `||||||| `).
fn is_conflict_marker(line: &str) -> bool {
    let line = line.trim_end();
    line == "======="
        || ["<<<<<<<", ">>>>>>>", "|||||||"]
            .iter()
            .any(|m| line == *m || line.starts_with(&format!("{m} ")))
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    // Finding #6: visibility must be read from the `**Visibility:**` field's
    // value on its own line, not via a substring `contains` over the whole
    // Scope section.
    #[test]
    fn visibility_reads_the_field_value() {
        // POSITIVE: explicit field values parse correctly.
        assert_eq!(
            visibility_from_scope_text("- **Visibility:** personal"),
            Visibility::Personal
        );
        assert_eq!(
            visibility_from_scope_text("- **Visibility:** shared"),
            Visibility::Shared
        );
        // POSITIVE: absent field defaults to Shared.
        assert_eq!(
            visibility_from_scope_text("- **Files:** `/`"),
            Visibility::Shared
        );
    }

    #[test]
    fn visibility_not_flipped_by_substring_in_other_text() {
        // NEGATIVE (red before fix): the real Visibility field says shared, but
        // another line merely *mentions* the personal-visibility text. A bare
        // `contains` would wrongly flip this memory to Personal.
        let scope = "- **Logical:** `note about **Visibility:** personal mode`\n\
                     - **Visibility:** shared";
        assert_eq!(
            visibility_from_scope_text(scope),
            Visibility::Shared,
            "visibility must come from the field line, not any substring"
        );
    }
}
