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
    parse_list_field, parse_provenance_section, parse_score_field, split_frontmatter,
    HIDDEN_META_END, HIDDEN_META_START,
};
use super::{MemoryParser, MemoryWriter, CURRENT_FORMAT_VERSION};
use crate::error::Result;
use engram_types::{Challenge, Decay, Memory, MemoryType, Status, Visibility};

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
    decay: Option<Decay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    challenges: Vec<Challenge>,
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
    let fm: MinimalFrontmatter = serde_yaml_ng::from_str(frontmatter)?;

    let sections = parse_body_sections(body);

    let h1 = sections.get("__h1__").cloned().unwrap_or_default();
    // When title is set, H1 is the title and summary is in a **Summary:** line
    let summary = if fm.title.is_some() {
        extract_bold_field(body, "Summary").unwrap_or(h1)
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
    let hidden = parse_hidden_meta(body);

    Ok(Memory {
        id: fm.id,
        type_: fm.type_,
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
        challenges: hidden.challenges,
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
        title: memory.title.clone(),
    };
    let yaml = serde_yaml_ng::to_string(&fm)?;
    out.push_str(&yaml);
    out.push_str("---\n\n");

    // -- H1: title (if set) or summary --
    let heading = memory.title.as_deref().unwrap_or(&memory.summary);
    out.push_str(&format!("# {heading}\n\n"));

    // When title is used as H1, write summary separately so it round-trips
    if memory.title.is_some() {
        out.push_str(&format!("**Summary:** {}\n\n", memory.summary));
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
    out.push_str("\n## Scope\n\n");
    if !memory.physical.is_empty() {
        let paths: Vec<String> = memory.physical.iter().map(|p| format!("`{p}`")).collect();
        out.push_str(&format!("- **Files:** {}\n", paths.join(", ")));
    }
    if !memory.logical.is_empty() {
        let scopes: Vec<String> = memory.logical.iter().map(|l| format!("`{l}`")).collect();
        out.push_str(&format!("- **Logical:** {}\n", scopes.join(", ")));
    }
    if !memory.tags.is_empty() {
        out.push_str(&format!("- **Tags:** {}\n", memory.tags.join(", ")));
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
    if let Some(ref agent_id) = memory.provenance.agent_id {
        out.push_str(&format!("- **Agent:** {agent_id}\n"));
    }
    if let Some(ref model) = memory.provenance.model {
        out.push_str(&format!("- **Model:** {model}\n"));
    }
    if let Some(ref session_id) = memory.provenance.session_id {
        out.push_str(&format!("- **Session:** {session_id}\n"));
    }
    if let Some(ref reason) = memory.provenance.reason {
        out.push_str(&format!("- **Reason:** {reason}\n"));
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
        decay: memory.decay.clone(),
        supersedes: memory.supersedes.clone(),
        challenges: memory.challenges.clone(),
    };
    let hidden_yaml = serde_yaml_ng::to_string(&hidden)?;
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
fn parse_hidden_meta(body: &str) -> HiddenMeta {
    let lines: Vec<&str> = body.lines().collect();
    if let Some(start) = lines
        .iter()
        .rposition(|line| line.trim_end() == HIDDEN_META_START)
    {
        if let Some(len) = lines[start + 1..]
            .iter()
            .position(|line| line.trim_end() == HIDDEN_META_END)
        {
            let yaml_content = lines[start + 1..start + 1 + len].join("\n");
            if let Ok(meta) = serde_yaml_ng::from_str(yaml_content.trim()) {
                return meta;
            }
        }
    }

    // Legacy fallback: first substring occurrence anywhere in the body.
    if let Some(start_idx) = body.find(HIDDEN_META_START) {
        let after_marker = &body[start_idx + HIDDEN_META_START.len()..];
        if let Some(end_idx) = after_marker.find(HIDDEN_META_END) {
            let yaml_content = after_marker[..end_idx].trim();
            if let Ok(meta) = serde_yaml_ng::from_str(yaml_content) {
                return meta;
            }
        }
    }

    HiddenMeta::default()
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
