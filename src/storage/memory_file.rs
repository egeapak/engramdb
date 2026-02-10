//! Frontmatter markdown parser and writer for memory files

use crate::types::Memory;
use super::error::{Result, StorageError};

/// Parse a memory file in frontmatter markdown format:
/// ```
/// ---
/// <YAML frontmatter>
/// ---
///
/// ## Content
///
/// <content text>
///
/// ## Details
///
/// <optional details text>
/// ```
pub fn parse_memory_file(content: &str) -> Result<Memory> {
    let mut parts = content.splitn(3, "---");

    // Skip first empty part
    parts.next();

    // Get frontmatter
    let frontmatter = parts.next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing frontmatter".to_string()))?
        .trim();

    // Get body
    let body = parts.next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing body after frontmatter".to_string()))?;

    // Parse frontmatter as YAML
    let mut memory: Memory = serde_yaml::from_str(frontmatter)?;

    // Parse body sections
    let sections = parse_body_sections(body);

    if let Some(content) = sections.get("Content") {
        memory.content = content.clone();
    }

    if let Some(details) = sections.get("Details") {
        memory.details = Some(details.clone());
    }

    Ok(memory)
}

/// Parse the body sections (## Content, ## Details)
fn parse_body_sections(body: &str) -> std::collections::HashMap<String, String> {
    let mut sections = std::collections::HashMap::new();
    let mut current_section: Option<String> = None;
    let mut current_content = Vec::new();

    for line in body.lines() {
        if line.starts_with("## ") {
            // Save previous section if any
            if let Some(section_name) = current_section.take() {
                sections.insert(section_name, current_content.join("\n").trim().to_string());
                current_content.clear();
            }

            // Start new section
            current_section = Some(line[3..].trim().to_string());
        } else if current_section.is_some() {
            current_content.push(line);
        }
    }

    // Save last section
    if let Some(section_name) = current_section {
        sections.insert(section_name, current_content.join("\n").trim().to_string());
    }

    sections
}

/// Write a memory to frontmatter markdown format
pub fn write_memory_file(memory: &Memory) -> Result<String> {
    let mut output = String::new();

    // Write frontmatter
    output.push_str("---\n");
    let yaml = serde_yaml::to_string(memory)?;
    output.push_str(&yaml);
    output.push_str("---\n\n");

    // Write content section
    output.push_str("## Content\n\n");
    output.push_str(&memory.content);
    output.push_str("\n");

    // Write details section if present
    if let Some(details) = &memory.details {
        output.push_str("\n## Details\n\n");
        output.push_str(details);
        output.push_str("\n");
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let original = r#"---
id: test-123
type: hazard
summary: Test memory
content: ""
status: active
visibility: shared
created_at: "2026-01-15T10:00:00Z"
updated_at: "2026-01-15T10:00:00Z"
---

## Content

This is the content.

## Details

These are the details.
"#;

        let memory = parse_memory_file(original).unwrap();
        assert_eq!(memory.id, "test-123");
        assert_eq!(memory.content, "This is the content.");
        assert_eq!(memory.details.as_deref(), Some("These are the details."));

        let written = write_memory_file(&memory).unwrap();
        let reparsed = parse_memory_file(&written).unwrap();

        assert_eq!(memory.id, reparsed.id);
        assert_eq!(memory.content, reparsed.content);
        assert_eq!(memory.details, reparsed.details);
    }
}
