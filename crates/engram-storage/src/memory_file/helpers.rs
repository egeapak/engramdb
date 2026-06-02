//! Shared parsing helpers used by multiple format versions.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use engram_types::{Provenance, ProvenanceSource};

/// Parse the body into named sections. Recognizes:
/// - `# heading` → stored under key `__h1__`
/// - `> blockquote` → stored under key `__blockquote__`
/// - `## SectionName` → stored under key `SectionName`
/// - `<!-- engramdb ... -->` → handled separately by `parse_hidden_meta`
pub fn parse_body_sections(body: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
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
    sections: &mut HashMap<String, String>,
    current_section: &mut Option<String>,
    current_content: &mut Vec<&str>,
) {
    if let Some(name) = current_section.take() {
        sections.insert(name, current_content.join("\n").trim().to_string());
        current_content.clear();
    }
}

/// Parse a numeric score from text like `- **Criticality:** 0.95`
pub fn parse_score_field(text: &str, field: &str) -> Option<f64> {
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
pub fn parse_list_field(text: &str, field: &str) -> Vec<String> {
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

/// Parse a datetime from a line like `- **Created:** 2026-01-15T10:00:00Z`
pub fn parse_datetime_field(text: &str, field: &str) -> Option<DateTime<Utc>> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(pos) = line.find(&marker) {
            let after = line[pos + marker.len()..].trim();
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
pub fn parse_string_field(text: &str, field: &str) -> Option<String> {
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
pub fn parse_provenance_section(text: &str) -> Provenance {
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

/// Format a provenance source to its string representation.
pub fn format_provenance_source(source: ProvenanceSource) -> &'static str {
    match source {
        ProvenanceSource::Human => "human",
        ProvenanceSource::Agent => "agent",
        ProvenanceSource::Inferred => "inferred",
        ProvenanceSource::Imported => "imported",
    }
}
