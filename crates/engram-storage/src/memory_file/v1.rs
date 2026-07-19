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

use super::helpers::{escape_body_text, parse_body_sections, split_frontmatter};
use super::{MemoryParser, MemoryWriter};
use crate::error::Result;
use engram_types::Memory;

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

fn parse_v1(frontmatter: &str, body: &str) -> Result<Memory> {
    let mut memory: Memory = serde_yaml_ng::from_str(frontmatter)?;
    super::validate_id_shape(&memory.id)?;

    // Serde's field default for `epistemic` is `Fact`, but the authoritative
    // defaulting rule for files that predate the field is type-derived
    // (`Debug` ⇒ Observation, `Decision`/`Intent`/`Preference` ⇒ Decision).
    // Serde can't distinguish "key absent" from "key = fact", so re-check the
    // raw frontmatter for the key.
    let has_epistemic_key = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(frontmatter)
        .ok()
        .and_then(|v| {
            v.as_mapping()
                .map(|m| m.contains_key(serde_yaml_ng::Value::from("epistemic")))
        })
        .unwrap_or(false);
    if !has_epistemic_key {
        memory.epistemic = memory.type_.default_epistemic();
    }
    let sections = parse_body_sections(body);

    // H1 title in body overrides frontmatter title
    if let Some(title) = sections.get("__h1__") {
        memory.title = Some(title.clone());
    }

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
    let yaml = serde_yaml_ng::to_string(&yaml_memory)?;
    out.push_str(&yaml);
    out.push_str("---\n\n");

    // Write title as H1 if present
    if let Some(ref title) = memory.title {
        out.push_str("# ");
        out.push_str(title);
        out.push_str("\n\n");
    }

    out.push_str("## Content\n\n");
    out.push_str(&escape_body_text(&memory.content));
    out.push('\n');

    if let Some(details) = &memory.details {
        out.push_str("\n## Details\n\n");
        out.push_str(&escape_body_text(details));
        out.push('\n');
    }

    Ok(out)
}
