//! Frontmatter markdown parser and writer for memory files.
//!
//! This module handles serialization and deserialization of memory files
//! in frontmatter markdown format:
//! ```text
//! ---
//! <YAML frontmatter with all memory fields>
//! ---
//!
//! ## Content
//! <main content text>
//!
//! ## Details
//! <optional extended details>
//! ```
//!
//! The frontmatter contains all Memory struct fields as YAML, while the body
//! sections provide human-readable content and details. This format is both
//! machine-readable and human-editable, enabling manual memory curation.

use super::error::{Result, StorageError};
use crate::types::Memory;

/// Parse a memory file in frontmatter markdown format:
/// ```text
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
    let frontmatter = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing frontmatter".to_string()))?
        .trim();

    // Get body
    let body = parts
        .next()
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
        if let Some(header) = line.strip_prefix("## ") {
            // Save previous section if any
            if let Some(section_name) = current_section.take() {
                sections.insert(section_name, current_content.join("\n").trim().to_string());
                current_content.clear();
            }

            // Start new section
            current_section = Some(header.trim().to_string());
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
    output.push('\n');

    // Write details section if present
    if let Some(details) = &memory.details {
        output.push_str("\n## Details\n\n");
        output.push_str(details);
        output.push('\n');
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

    #[test]
    fn test_parse_without_details() {
        let input = r#"---
id: test-456
type: decision
summary: Test without details
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

Main content only.
"#;

        let memory = parse_memory_file(input).unwrap();
        assert_eq!(memory.id, "test-456");
        assert_eq!(memory.content, "Main content only.");
        assert_eq!(memory.details, None);
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
        let input = r#"---
id: test-789
type: context
summary: Multiline test
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

First paragraph here.

Second paragraph here.

Third paragraph here.
"#;

        let memory = parse_memory_file(input).unwrap();
        assert_eq!(memory.id, "test-789");
        assert!(memory.content.contains("First paragraph"));
        assert!(memory.content.contains("Second paragraph"));
        assert!(memory.content.contains("Third paragraph"));
        // Check that newlines are preserved
        assert!(memory.content.contains("\n\n"));
    }

    #[test]
    fn test_write_without_details() {
        use crate::types::{Memory, MemoryType, Provenance};

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
            status: crate::types::Status::Active,
            visibility: crate::types::Visibility::Shared,
            challenges: vec![],
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
        let input = r#"---
id: test-empty
type: debug
summary: Empty content test
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

"#;

        let memory = parse_memory_file(input).unwrap();
        assert_eq!(memory.id, "test-empty");
        assert_eq!(memory.content, "");
    }

    #[test]
    fn test_parse_invalid_yaml_frontmatter() {
        let input = r#"---
id: test-invalid
this is not valid yaml: {{{
malformed: [unclosed
---

## Content

Some content here.
"#;

        let result = parse_memory_file(input);
        assert!(result.is_err());
        match result {
            Err(StorageError::Yaml(_)) => {
                // Correct error type
            }
            _ => panic!("Expected Yaml error for invalid frontmatter"),
        }
    }

    #[test]
    fn test_parse_content_with_triple_dashes() {
        let input = r#"---
id: test-dashes
type: context
summary: Content with dashes
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

This content has triple dashes:

---

And it should still parse correctly.

## Details

Even in details section:
---
No problem!
"#;

        let memory = parse_memory_file(input).unwrap();
        assert_eq!(memory.id, "test-dashes");
        assert!(memory.content.contains("---"));
        assert!(memory
            .content
            .contains("And it should still parse correctly"));
        assert!(memory.details.is_some());
        let details = memory.details.unwrap();
        assert!(details.contains("---"));
        assert!(details.contains("No problem!"));
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
}
