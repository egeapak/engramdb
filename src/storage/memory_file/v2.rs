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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::helpers::{
    format_provenance_source, parse_body_sections, parse_datetime_field, parse_list_field,
    parse_provenance_section, parse_score_field,
};
use super::{MemoryParser, MemoryWriter, CURRENT_FORMAT_VERSION};
use crate::storage::error::{Result, StorageError};
use crate::types::{Challenge, Decay, Memory, MemoryType, Status, Visibility};

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

fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let mut parts = content.splitn(3, "---");
    parts.next();
    let frontmatter = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing frontmatter".to_string()))?
        .trim();
    let body = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing body after frontmatter".to_string()))?;
    Ok((frontmatter, body))
}

fn parse_v2(frontmatter: &str, body: &str) -> Result<Memory> {
    let fm: MinimalFrontmatter = serde_yml::from_str(frontmatter)?;

    let sections = parse_body_sections(body);

    let summary = sections.get("__h1__").cloned().unwrap_or_default();
    let content = sections.get("Content").cloned().unwrap_or_default();
    let details = sections.get("Details").cloned().filter(|s| !s.is_empty());

    // Parse scope
    let scope_text = sections.get("Scope").map(String::as_str).unwrap_or("");
    let physical = parse_list_field(scope_text, "Files");
    let logical = parse_list_field(scope_text, "Logical");
    let tags = parse_list_field(scope_text, "Tags");
    let criticality = parse_score_field(scope_text, "Criticality").unwrap_or(0.5);
    let confidence = parse_score_field(scope_text, "Confidence").unwrap_or(0.8);
    let visibility_from_scope = if scope_text.contains("**Visibility:** personal") {
        Visibility::Personal
    } else {
        Visibility::Shared
    };

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
    };
    let yaml = serde_yml::to_string(&fm)?;
    out.push_str(&yaml);
    out.push_str("---\n\n");

    // -- H1: summary --
    out.push_str(&format!("# {}\n\n", memory.summary));

    // -- ## Content --
    out.push_str("## Content\n\n");
    out.push_str(&memory.content);
    out.push('\n');

    // -- ## Details (optional) --
    if let Some(details) = &memory.details {
        out.push_str("\n## Details\n\n");
        out.push_str(details);
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
    let hidden_yaml = serde_yml::to_string(&hidden)?;
    out.push_str(&format!("\n<!-- engramdb\n{hidden_yaml}-->\n"));

    Ok(out)
}

/// Extract `<!-- engramdb ... -->` block and parse as YAML.
fn parse_hidden_meta(body: &str) -> HiddenMeta {
    let start_marker = "<!-- engramdb";
    let end_marker = "-->";

    if let Some(start_idx) = body.find(start_marker) {
        let after_marker = &body[start_idx + start_marker.len()..];
        if let Some(end_idx) = after_marker.find(end_marker) {
            let yaml_content = after_marker[..end_idx].trim();
            if let Ok(meta) = serde_yml::from_str(yaml_content) {
                return meta;
            }
        }
    }

    HiddenMeta::default()
}
