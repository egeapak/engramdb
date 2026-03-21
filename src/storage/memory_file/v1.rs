//! V1 (legacy) format: full YAML frontmatter with all Memory fields.
//!
//! ```text
//! ---
//! id: <uuid>
//! type: <memory_type>
//! summary: <summary>
//! content: ""
//! physical: [...]
//! ... (all Memory fields as YAML)
//! ---
//!
//! ## Content
//! <content text>
//!
//! ## Details
//! <optional details>
//! ```

use super::helpers::parse_body_sections;
use super::{MemoryParser, MemoryWriter};
use crate::storage::error::{Result, StorageError};
use crate::types::Memory;

/// Parser for the V1 (legacy) full-YAML-frontmatter format.
pub struct V1Parser;

impl MemoryParser for V1Parser {
    fn parse(&self, content: &str) -> Result<Memory> {
        let (frontmatter, body) = split_frontmatter(content)?;
        parse_v1(frontmatter, body)
    }

    fn version(&self) -> Option<u32> {
        None
    }
}

/// Writer that produces the V1 (legacy) full-YAML-frontmatter format.
pub struct V1Writer;

impl MemoryWriter for V1Writer {
    fn write(&self, memory: &Memory) -> Result<String> {
        write_v1(memory)
    }

    fn version(&self) -> Option<u32> {
        None
    }
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let mut parts = content.splitn(3, "---");
    parts.next(); // skip before first ---
    let frontmatter = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing frontmatter".to_string()))?
        .trim();
    let body = parts
        .next()
        .ok_or_else(|| StorageError::InvalidFormat("Missing body after frontmatter".to_string()))?;
    Ok((frontmatter, body))
}

fn parse_v1(frontmatter: &str, body: &str) -> Result<Memory> {
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

fn write_v1(memory: &Memory) -> Result<String> {
    let mut out = String::new();

    out.push_str("---\n");
    // Serialize all fields as YAML (the legacy format).
    // We serialize a copy with content/details blanked, since those go in the body.
    let mut yaml_memory = memory.clone();
    yaml_memory.content = String::new();
    yaml_memory.details = None;
    let yaml = serde_yml::to_string(&yaml_memory)?;
    out.push_str(&yaml);
    out.push_str("---\n");

    out.push_str("\n## Content\n\n");
    out.push_str(&memory.content);
    out.push('\n');

    if let Some(details) = &memory.details {
        out.push_str("\n## Details\n\n");
        out.push_str(details);
        out.push('\n');
    }

    Ok(out)
}
