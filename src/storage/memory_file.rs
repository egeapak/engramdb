//! Structured markdown parser and writer for memory files.
//!
//! Memory files use a minimal YAML frontmatter for machine identity fields,
//! with all other metadata rendered as human-readable markdown sections:
//!
//! ```text
//! ---
//! id: <uuid>
//! type: <memory_type>
//! status: <status>
//! ---
//!
//! # <summary>
//!
//! ## Content
//!
//! <main content text>
//!
//! ## Details
//!
//! <optional extended details>
//!
//! ## Scope
//!
//! - **Files:** `src/db/**`
//! - **Tags:** database, transactions
//! - **Criticality:** 0.95
//! - **Confidence:** 1.0
//!
//! ## Provenance
//!
//! - **Source:** human
//! - **Created:** 2026-01-15T10:00:00Z
//!
//! <!-- engramdb
//! visibility: shared
//! accessed_at: "2026-01-15T10:00:00Z"
//! -->
//! ```
//!
//! This format is both machine-readable and human-editable, enabling manual
//! memory curation while keeping files pleasant to read in any markdown viewer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::error::{Result, StorageError};
use crate::types::{
    Challenge, Decay, Memory, MemoryType, Provenance, ProvenanceSource, Status, Visibility,
};

// ---------------------------------------------------------------------------
// Minimal frontmatter (YAML between --- fences)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct MinimalFrontmatter {
    id: String,
    #[serde(rename = "type")]
    type_: MemoryType,
    status: Status,
}

// ---------------------------------------------------------------------------
// Hidden metadata block (YAML inside <!-- engramdb ... -->)
// ---------------------------------------------------------------------------

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

// ===========================================================================
// Writer
// ===========================================================================

/// Write a memory to the structured markdown format.
pub fn write_memory_file(memory: &Memory) -> Result<String> {
    let mut out = String::new();

    // -- Minimal YAML frontmatter --
    out.push_str("---\n");
    let fm = MinimalFrontmatter {
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

fn format_provenance_source(source: ProvenanceSource) -> &'static str {
    match source {
        ProvenanceSource::Human => "human",
        ProvenanceSource::Agent => "agent",
        ProvenanceSource::Inferred => "inferred",
        ProvenanceSource::Imported => "imported",
    }
}

// ===========================================================================
// Parser
// ===========================================================================

/// Parse a memory file. Supports both the new structured format and the
/// legacy full-YAML-frontmatter format for backward compatibility.
pub fn parse_memory_file(content: &str) -> Result<Memory> {
    let mut parts = content.splitn(3, "---");

    // Skip leading text before first ---
    parts.next();

    let frontmatter = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing frontmatter".to_string()))?
        .trim();

    let body = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing body after frontmatter".to_string()))?;

    // Detect format: if the frontmatter contains a "summary" key it's legacy
    if frontmatter.contains("\nsummary:")
        || frontmatter.starts_with("summary:")
        || frontmatter.contains("\ncontent:")
        || frontmatter.starts_with("content:")
    {
        return parse_legacy(frontmatter, body);
    }

    parse_structured(frontmatter, body)
}

/// Parse the new structured format.
fn parse_structured(frontmatter: &str, body: &str) -> Result<Memory> {
    let fm: MinimalFrontmatter = serde_yml::from_str(frontmatter)?;

    let sections = parse_body_sections(body);

    // H1 heading = summary (captured as section with key "")
    let summary = sections.get("__h1__").cloned().unwrap_or_default();

    let content = sections.get("Content").cloned().unwrap_or_default();
    let details = sections.get("Details").cloned().filter(|s| !s.is_empty());

    // Parse scope (includes criticality, confidence, visibility)
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

/// Parse the legacy full-YAML frontmatter format.
fn parse_legacy(frontmatter: &str, body: &str) -> Result<Memory> {
    let mut memory: Memory = serde_yml::from_str(frontmatter)?;
    let sections = parse_body_sections(body);

    if let Some(content) = sections.get("Content") {
        memory.content = content.clone();
    }
    if let Some(details) = sections.get("Details") {
        memory.details = Some(details.clone());
    }

    Ok(memory)
}

// ---------------------------------------------------------------------------
// Section parsing helpers
// ---------------------------------------------------------------------------

/// Parse the body into named sections. Recognizes:
/// - `# heading` → stored under key `__h1__`
/// - `> blockquote` → stored under key `__blockquote__`
/// - `## SectionName` → stored under key `SectionName`
/// - `<!-- engramdb ... -->` → handled separately by parse_hidden_meta
fn parse_body_sections(body: &str) -> std::collections::HashMap<String, String> {
    let mut sections = std::collections::HashMap::new();
    let mut current_section: Option<String> = None;
    let mut current_content = Vec::new();

    for line in body.lines() {
        // H1 heading
        if let Some(heading) = line.strip_prefix("# ") {
            flush_section(&mut sections, &mut current_section, &mut current_content);
            sections.insert("__h1__".to_string(), heading.trim().to_string());
            continue;
        }

        // Blockquote (collect all `>` lines)
        if line.starts_with("> ") || line == ">" {
            let text = line.strip_prefix("> ").unwrap_or("").trim();
            if let Some(existing) = sections.get_mut("__blockquote__") {
                existing.push(' ');
                existing.push_str(text);
            } else {
                sections.insert("__blockquote__".to_string(), text.to_string());
            }
            continue;
        }

        // H2 section
        if let Some(header) = line.strip_prefix("## ") {
            flush_section(&mut sections, &mut current_section, &mut current_content);
            current_section = Some(header.trim().to_string());
            continue;
        }

        // Skip HTML comment lines (handled by parse_hidden_meta)
        if line.starts_with("<!--") || line.starts_with("-->") {
            continue;
        }

        // Accumulate lines into current section
        if current_section.is_some() {
            current_content.push(line);
        }
    }

    flush_section(&mut sections, &mut current_section, &mut current_content);
    sections
}

fn flush_section(
    sections: &mut std::collections::HashMap<String, String>,
    current_section: &mut Option<String>,
    current_content: &mut Vec<&str>,
) {
    if let Some(name) = current_section.take() {
        sections.insert(name, current_content.join("\n").trim().to_string());
        current_content.clear();
    }
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

/// Parse a numeric score from text like `- **Criticality:** 0.95`
fn parse_score_field(text: &str, field: &str) -> Option<f64> {
    let marker = format!("**{field}:**");
    let pos = text.find(&marker)?;
    let after = &text[pos + marker.len()..];
    let value_str = after
        .trim_start()
        .split(|c: char| c == '|' || c == '*' || c.is_whitespace())
        .next()?
        .trim();
    value_str.parse().ok()
}

/// Parse a markdown list field like `- **Files:** \`src/db/**\`, \`src/lib.rs\``
/// Returns individual items (backtick-wrapped items are unwrapped).
fn parse_list_field(text: &str, field: &str) -> Vec<String> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(pos) = line.find(&marker) {
            let after = &line[pos + marker.len()..];
            return after
                .split(',')
                .map(|s| s.trim().trim_matches('`').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    vec![]
}

/// Parse a datetime from a provenance-style line like `- **Created:** 2026-01-15T10:00:00Z`
fn parse_datetime_field(text: &str, field: &str) -> Option<DateTime<Utc>> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(pos) = line.find(&marker) {
            let after = &line[pos + marker.len()..].trim();
            if let Ok(dt) = DateTime::parse_from_rfc3339(after) {
                return Some(dt.with_timezone(&Utc));
            }
            // Try chrono's more lenient parsing
            if let Ok(dt) = after.parse::<DateTime<Utc>>() {
                return Some(dt);
            }
        }
    }
    None
}

/// Parse a simple string field from `- **Key:** value` lines.
fn parse_string_field(text: &str, field: &str) -> Option<String> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(pos) = line.find(&marker) {
            let val = line[pos + marker.len()..].trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Parse the ## Provenance section into a Provenance struct.
fn parse_provenance_section(text: &str) -> Provenance {
    let source_str = parse_string_field(text, "Source").unwrap_or_else(|| "human".to_string());
    let source = match source_str.as_str() {
        "agent" => ProvenanceSource::Agent,
        "inferred" => ProvenanceSource::Inferred,
        "imported" => ProvenanceSource::Imported,
        _ => ProvenanceSource::Human,
    };

    Provenance {
        source,
        agent_id: parse_string_field(text, "Agent"),
        model: parse_string_field(text, "Model"),
        session_id: parse_string_field(text, "Session"),
        reason: parse_string_field(text, "Reason"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Decay;

    #[test]
    fn test_roundtrip() {
        let memory = Memory {
            id: "test-123".to_string(),
            type_: MemoryType::Hazard,
            summary: "Test memory".to_string(),
            content: "This is the content.".to_string(),
            details: Some("These are the details.".to_string()),
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: Some(Decay::none_with_floor(0.5)),
            provenance: Provenance::human(),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        let reparsed = parse_memory_file(&written).unwrap();

        assert_eq!(memory.id, reparsed.id);
        assert_eq!(memory.type_, reparsed.type_);
        assert_eq!(memory.summary, reparsed.summary);
        assert_eq!(memory.content, reparsed.content);
        assert_eq!(memory.details, reparsed.details);
        assert_eq!(memory.status, reparsed.status);
        assert_eq!(memory.visibility, reparsed.visibility);
        assert_eq!(memory.criticality, reparsed.criticality);
        assert_eq!(memory.confidence, reparsed.confidence);
        assert_eq!(memory.physical, reparsed.physical);
        assert_eq!(memory.provenance.source, reparsed.provenance.source);
    }

    #[test]
    fn test_roundtrip_with_all_fields() {
        let memory = Memory {
            id: "test-full".to_string(),
            type_: MemoryType::Decision,
            summary: "Full field test".to_string(),
            content: "Content here.".to_string(),
            details: Some("Details here.".to_string()),
            physical: vec!["src/db/**".to_string(), "src/lib.rs".to_string()],
            logical: vec!["infrastructure.database".to_string()],
            tags: vec!["database".to_string(), "transactions".to_string()],
            criticality: 0.95,
            decay: Some(Decay::exponential(chrono::Duration::days(14))),
            provenance: Provenance::agent("claude")
                .with_model("claude-opus-4-6")
                .with_session("sess-123")
                .with_reason("Post-incident review"),
            confidence: 1.0,
            supersedes: vec!["old-id-1".to_string()],
            status: Status::Active,
            visibility: Visibility::Personal,
            challenges: vec![],
            verified_at: None,
            created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        let reparsed = parse_memory_file(&written).unwrap();

        assert_eq!(memory.id, reparsed.id);
        assert_eq!(memory.summary, reparsed.summary);
        assert_eq!(memory.physical, reparsed.physical);
        assert_eq!(memory.logical, reparsed.logical);
        assert_eq!(memory.tags, reparsed.tags);
        assert_eq!(memory.criticality, reparsed.criticality);
        assert_eq!(memory.confidence, reparsed.confidence);
        assert_eq!(memory.visibility, reparsed.visibility);
        assert_eq!(memory.provenance.source, reparsed.provenance.source);
        assert_eq!(memory.provenance.agent_id, reparsed.provenance.agent_id);
        assert_eq!(memory.provenance.model, reparsed.provenance.model);
        assert_eq!(memory.provenance.session_id, reparsed.provenance.session_id);
        assert_eq!(memory.provenance.reason, reparsed.provenance.reason);
        assert_eq!(memory.supersedes, reparsed.supersedes);
    }

    #[test]
    fn test_parse_legacy_format() {
        let input = r#"---
id: test-123
type: hazard
summary: Test memory
content: ""
physical:
  - "/"
logical: []
tags: []
criticality: 0.5
provenance:
  source: human
confidence: 0.8
supersedes: []
status: Active
visibility: Shared
challenges: []
created_at: "2026-01-15T10:00:00Z"
updated_at: "2026-01-15T10:00:00Z"
accessed_at: "2026-01-15T10:00:00Z"
---

## Content

This is the content.

## Details

These are the details.
"#;

        let memory = parse_memory_file(input).unwrap();
        assert_eq!(memory.id, "test-123");
        assert_eq!(memory.content, "This is the content.");
        assert_eq!(memory.details.as_deref(), Some("These are the details."));
        assert_eq!(memory.criticality, 0.5);
    }

    #[test]
    fn test_parse_without_details() {
        let memory = Memory {
            id: "test-456".to_string(),
            type_: MemoryType::Decision,
            summary: "Test without details".to_string(),
            content: "Main content only.".to_string(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        assert!(written.contains("## Content"));
        assert!(!written.contains("## Details"));

        let reparsed = parse_memory_file(&written).unwrap();
        assert_eq!(reparsed.content, "Main content only.");
        assert_eq!(reparsed.details, None);
    }

    #[test]
    fn test_parse_missing_frontmatter() {
        let input = r#"
## Content

No frontmatter here.
"#;

        let result = parse_memory_file(input);
        assert!(result.is_err());
        match result {
            Err(StorageError::InvalidFormat(msg)) => {
                assert!(msg.contains("Missing frontmatter"));
            }
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    #[test]
    fn test_parse_multiline_content() {
        let memory = Memory {
            id: "test-789".to_string(),
            type_: MemoryType::Context,
            summary: "Multiline test".to_string(),
            content: "First paragraph here.\n\nSecond paragraph here.\n\nThird paragraph here."
                .to_string(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        let reparsed = parse_memory_file(&written).unwrap();

        assert!(reparsed.content.contains("First paragraph"));
        assert!(reparsed.content.contains("Second paragraph"));
        assert!(reparsed.content.contains("Third paragraph"));
        assert!(reparsed.content.contains("\n\n"));
    }

    #[test]
    fn test_write_without_details() {
        let memory = Memory {
            id: "test-write-1".to_string(),
            type_: MemoryType::Hazard,
            summary: "Test summary".to_string(),
            content: "Test content".to_string(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            accessed_at: chrono::Utc::now(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        assert!(written.contains("## Content"));
        assert!(!written.contains("## Details"));
    }

    #[test]
    fn test_parse_empty_content_section() {
        let memory = Memory {
            id: "test-empty".to_string(),
            type_: MemoryType::Debug,
            summary: "Empty content test".to_string(),
            content: String::new(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            accessed_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        let reparsed = parse_memory_file(&written).unwrap();
        assert_eq!(reparsed.id, "test-empty");
        assert_eq!(reparsed.content, "");
    }

    #[test]
    fn test_parse_empty_input() {
        let input = "";

        let result = parse_memory_file(input);
        assert!(result.is_err());
        match result {
            Err(StorageError::InvalidFormat(msg)) => {
                assert!(msg.contains("Missing frontmatter") || msg.contains("Missing body"));
            }
            _ => panic!("Expected InvalidFormat error for empty input"),
        }
    }

    #[test]
    fn test_output_format_readable() {
        let memory = Memory {
            id: "0195a3b7-8c4d-7e2f-a1b3-9d4e5f6a7b8c".to_string(),
            type_: MemoryType::Hazard,
            summary: "Never call sync() outside a transaction".to_string(),
            content: "The sync() method acquires a write lock.".to_string(),
            details: Some("Incident on 2026-01-14.".to_string()),
            physical: vec!["src/db/**".to_string()],
            logical: vec!["infrastructure.database".to_string()],
            tags: vec![
                "database".to_string(),
                "transactions".to_string(),
                "deadlock".to_string(),
            ],
            criticality: 0.95,
            decay: Some(Decay::none_with_floor(0.5)),
            provenance: Provenance::human().with_reason("Post-incident review of INC-2026-003"),
            confidence: 1.0,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            updated_at: "2026-01-15T10:00:00Z".parse().unwrap(),
            accessed_at: "2026-02-08T14:30:00Z".parse().unwrap(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();

        // Verify the structure is human-readable
        assert!(written.contains("# Never call sync() outside a transaction"));
        assert!(written.contains("- **Criticality:** 0.95"));
        assert!(written.contains("- **Confidence:** 1"));
        assert!(written.contains("## Content"));
        assert!(written.contains("## Details"));
        assert!(written.contains("## Scope"));
        assert!(written.contains("- **Files:** `src/db/**`"));
        assert!(written.contains("- **Tags:** database, transactions, deadlock"));
        assert!(written.contains("## Provenance"));
        assert!(written.contains("- **Source:** human"));
        assert!(written.contains("- **Reason:** Post-incident review of INC-2026-003"));
        assert!(written.contains("<!-- engramdb"));

        // Verify no ugly YAML dump of the entire struct
        assert!(!written.contains("content: \"\""));
        assert!(!written.contains("supersedes: []"));
    }

    #[test]
    fn test_personal_visibility_shown_in_scope() {
        let memory = Memory {
            id: "test-vis".to_string(),
            type_: MemoryType::Preference,
            summary: "Personal pref".to_string(),
            content: "My preference.".to_string(),
            details: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Personal,
            challenges: vec![],
            verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            accessed_at: chrono::Utc::now(),
            expires_at: None,
        };

        let written = write_memory_file(&memory).unwrap();
        assert!(written.contains("**Visibility:** personal"));

        let reparsed = parse_memory_file(&written).unwrap();
        assert_eq!(reparsed.visibility, Visibility::Personal);
    }

    #[test]
    fn test_parse_score_field() {
        assert_eq!(
            parse_score_field("**Criticality:** 0.95 | **Confidence:** 1.0", "Criticality"),
            Some(0.95)
        );
        assert_eq!(
            parse_score_field("**Criticality:** 0.95 | **Confidence:** 1.0", "Confidence"),
            Some(1.0)
        );
        assert_eq!(parse_score_field("no scores here", "Criticality"), None);
    }

    #[test]
    fn test_parse_list_field() {
        let text = "- **Files:** `src/db/**`, `src/lib.rs`\n- **Tags:** database, transactions";
        assert_eq!(
            parse_list_field(text, "Files"),
            vec!["src/db/**", "src/lib.rs"]
        );
        assert_eq!(
            parse_list_field(text, "Tags"),
            vec!["database", "transactions"]
        );
        assert!(parse_list_field(text, "Logical").is_empty());
    }
}
