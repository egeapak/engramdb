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

/// Slugify a title string for use in filenames.
///
/// Converts to lowercase, replaces non-alphanumeric characters with hyphens,
/// collapses consecutive hyphens, trims leading/trailing hyphens, and truncates
/// to a maximum of 50 characters (breaking at the last hyphen boundary).
pub fn slugify(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens
    let mut collapsed = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push('-');
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }

    // Trim hyphens and truncate
    let trimmed = collapsed.trim_matches('-');
    if trimmed.len() <= 50 {
        trimmed.to_string()
    } else {
        // Break at last hyphen before 50 chars
        let truncated = &trimmed[..50];
        match truncated.rfind('-') {
            Some(pos) => truncated[..pos].to_string(),
            None => truncated.to_string(),
        }
    }
}

/// Generate a memory filename from the memory's title and ID.
///
/// If the memory has a title, the filename is `<slug>_<uuid>.md`.
/// Otherwise, falls back to `<uuid>.md` for backward compatibility.
pub fn memory_filename(memory: &Memory) -> String {
    if let Some(ref title) = memory.title {
        let slug = slugify(title);
        if slug.is_empty() {
            format!("{}.md", memory.id)
        } else {
            format!("{}_{}.md", slug, memory.id)
        }
    } else {
        format!("{}.md", memory.id)
    }
}

/// Extract the memory ID (UUID) from a file stem.
///
/// Handles both old format (`<uuid>`) and new format (`<slug>_<uuid>`).
/// UUID v7 is always 36 characters: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
pub fn extract_id_from_stem(stem: &str) -> &str {
    if let Some(pos) = stem.rfind('_') {
        let candidate = &stem[pos + 1..];
        if candidate.len() == 36 && candidate.chars().filter(|c| *c == '-').count() == 4 {
            return candidate;
        }
    }
    // Old format or no slug: entire stem is the UUID
    stem
}

/// Check if a file stem matches an ID or ID prefix.
///
/// Supports both old (`<uuid>`) and new (`<slug>_<uuid>`) filename formats.
pub fn stem_matches_id_prefix(stem: &str, id_prefix: &str) -> bool {
    let id_part = extract_id_from_stem(stem);
    id_part.starts_with(id_prefix)
}

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
    let mut memory: Memory = serde_yml::from_str(frontmatter)?;

    // Parse body sections (including optional H1 title)
    let (h1_title, sections) = parse_body_sections(body);

    // H1 title in body overrides frontmatter title (they should match,
    // but body is the human-visible one)
    if let Some(title) = h1_title {
        memory.title = Some(title);
    }

    if let Some(content) = sections.get("Content") {
        memory.content = content.clone();
    }

    if let Some(details) = sections.get("Details") {
        memory.details = Some(details.clone());
    }

    Ok(memory)
}

/// Parse the body sections (## Content, ## Details) and optional H1 title.
///
/// Returns `(Option<title>, sections_map)`.
fn parse_body_sections(body: &str) -> (Option<String>, std::collections::HashMap<String, String>) {
    let mut sections = std::collections::HashMap::new();
    let mut current_section: Option<String> = None;
    let mut current_content = Vec::new();
    let mut h1_title: Option<String> = None;

    for line in body.lines() {
        // Detect H1 title (# Title) — must not be ## heading
        if line.starts_with("# ") && !line.starts_with("## ") && h1_title.is_none() {
            h1_title = Some(line.strip_prefix("# ").unwrap().trim().to_string());
            continue;
        }

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

    (h1_title, sections)
}

/// Write a memory to frontmatter markdown format
pub fn write_memory_file(memory: &Memory) -> Result<String> {
    let mut output = String::new();

    // Write frontmatter
    output.push_str("---\n");
    let yaml = serde_yml::to_string(memory)?;
    output.push_str(&yaml);
    output.push_str("---\n\n");

    // Write title as H1 if present
    if let Some(ref title) = memory.title {
        output.push_str("# ");
        output.push_str(title);
        output.push_str("\n\n");
    }

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

    // -----------------------------------------------------------------------
    // Title and filename tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Use Snake Case"), "use-snake-case");
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  spaces  "), "spaces");
        assert_eq!(slugify("MixedCase_And-Stuff"), "mixedcase-and-stuff");
    }

    #[test]
    fn test_slugify_truncation() {
        let long = "a".repeat(60);
        let slug = slugify(&long);
        assert!(slug.len() <= 50);
    }

    #[test]
    fn test_slugify_truncation_at_word_boundary() {
        let title = "this is a title that is quite long and should be truncated at a word boundary";
        let slug = slugify(title);
        assert!(slug.len() <= 50);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn test_memory_filename_with_title() {
        use crate::types::{Memory, MemoryType, Provenance};

        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        memory.title = Some("Use Snake Case".to_string());
        let filename = memory_filename(&memory);
        assert!(filename.starts_with("use-snake-case_"));
        assert!(filename.ends_with(".md"));
        assert!(filename.contains(&memory.id));
    }

    #[test]
    fn test_memory_filename_without_title() {
        use crate::types::{Memory, MemoryType, Provenance};

        let memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        let filename = memory_filename(&memory);
        assert_eq!(filename, format!("{}.md", memory.id));
    }

    #[test]
    fn test_memory_filename_with_empty_title() {
        use crate::types::{Memory, MemoryType, Provenance};

        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        memory.title = Some("---".to_string()); // slugifies to empty
        let filename = memory_filename(&memory);
        // Falls back to uuid-only format
        assert_eq!(filename, format!("{}.md", memory.id));
    }

    #[test]
    fn test_extract_id_from_stem_new_format() {
        let stem = "use-snake-case_019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
        assert_eq!(
            extract_id_from_stem(stem),
            "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8"
        );
    }

    #[test]
    fn test_extract_id_from_stem_old_format() {
        let stem = "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
        assert_eq!(
            extract_id_from_stem(stem),
            "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8"
        );
    }

    #[test]
    fn test_stem_matches_id_prefix_new_format() {
        let stem = "use-snake-case_019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
        assert!(stem_matches_id_prefix(stem, "019f0d3e"));
        assert!(stem_matches_id_prefix(
            stem,
            "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8"
        ));
        assert!(!stem_matches_id_prefix(stem, "aaaa"));
    }

    #[test]
    fn test_stem_matches_id_prefix_old_format() {
        let stem = "019f0d3e-7660-788a-b1d0-c4e0f5a6b7c8";
        assert!(stem_matches_id_prefix(stem, "019f0d3e"));
        assert!(!stem_matches_id_prefix(stem, "use-snake"));
    }

    #[test]
    fn test_write_with_title() {
        use crate::types::{Memory, MemoryType, Provenance};

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        memory.title = Some("Database Decision".to_string());

        let written = write_memory_file(&memory).unwrap();
        assert!(written.contains("# Database Decision\n\n## Content"));
    }

    #[test]
    fn test_write_without_title_no_h1() {
        use crate::types::{Memory, MemoryType, Provenance};

        let memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );

        let written = write_memory_file(&memory).unwrap();
        // Should not have an H1 heading
        assert!(!written.contains("\n# "));
        assert!(written.contains("## Content"));
    }

    #[test]
    fn test_roundtrip_with_title() {
        use crate::types::{Memory, MemoryType, Provenance};

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        memory.title = Some("My Title".to_string());

        let written = write_memory_file(&memory).unwrap();
        let reparsed = parse_memory_file(&written).unwrap();

        assert_eq!(reparsed.title, Some("My Title".to_string()));
        assert_eq!(reparsed.content, "Test content");
        assert_eq!(reparsed.summary, "Test summary");
    }

    #[test]
    fn test_parse_without_title_backward_compat() {
        // Old-format file without title should parse with title = None
        let input = r#"---
id: test-no-title
type: decision
summary: Test without title
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
        assert_eq!(memory.title, None);
        assert_eq!(memory.content, "Main content only.");
    }
}
