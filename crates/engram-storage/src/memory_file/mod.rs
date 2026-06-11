//! Structured markdown parser and writer for memory files.
//!
//! This module uses a trait-based design with versioned implementations:
//!
//! - [`MemoryParser`] — trait for reading a raw string into a [`Memory`]
//! - [`MemoryWriter`] — trait for serializing a [`Memory`] to a string
//! - [`V1Parser`] / [`V1Writer`] — legacy full-YAML-frontmatter format
//! - [`V2Parser`] / [`V2Writer`] — structured markdown with minimal frontmatter
//!
//! The top-level [`parse_memory_file`] auto-detects the version and dispatches
//! to the correct parser. [`write_memory_file`] always writes the latest format.

mod helpers;
pub mod v1;
pub mod v2;

#[cfg(test)]
mod tests;

use super::error::Result;
use engram_types::Memory;

pub use v1::{V1Parser, V1Writer};
pub use v2::{V2Parser, V2Writer};

/// Current memory file format version.
pub const CURRENT_FORMAT_VERSION: u32 = 2;

// ===========================================================================
// Filename helpers
// ===========================================================================

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

// ===========================================================================
// Traits
// ===========================================================================

/// Trait for parsing a raw memory file string into a [`Memory`].
pub trait MemoryParser {
    /// Parse raw file content into a Memory.
    fn parse(&self, content: &str) -> Result<Memory>;

    /// The format version this parser handles.
    /// Returns `None` for legacy (v1) files without an explicit version field.
    fn version(&self) -> Option<u32>;
}

/// Trait for writing a [`Memory`] to a formatted string.
pub trait MemoryWriter {
    /// Serialize a Memory into the file format string.
    fn write(&self, memory: &Memory) -> Result<String>;

    /// The format version this writer produces.
    /// Returns `None` for legacy (v1) format.
    fn version(&self) -> Option<u32>;
}

// ===========================================================================
// Version detection
// ===========================================================================

/// Detect the format version of a memory file's raw content.
///
/// Returns `None` for legacy (v1) files that lack a version field,
/// or `Some(n)` for files with an explicit `version: n`.
pub fn detect_format_version(content: &str) -> Option<u32> {
    let (frontmatter, _body) = helpers::split_frontmatter(content).ok()?;

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("version:") {
            return val.trim().parse().ok();
        }
    }
    None
}

/// Return the appropriate parser for a given format version.
///
/// `None` → V1 (legacy), `Some(2)` → V2, etc.
pub fn parser_for_version(version: Option<u32>) -> Box<dyn MemoryParser> {
    match version {
        Some(2) => Box::new(V2Parser),
        _ => Box::new(V1Parser),
    }
}

/// Return the latest writer (always writes the current format version).
pub fn latest_writer() -> Box<dyn MemoryWriter> {
    Box::new(V2Writer)
}

/// Return a writer for a specific format version.
pub fn writer_for_version(version: Option<u32>) -> Box<dyn MemoryWriter> {
    match version {
        Some(2) => Box::new(V2Writer),
        _ => Box::new(V1Writer),
    }
}

// ===========================================================================
// Convenience dispatch functions (backward-compatible API)
// ===========================================================================

/// Parse a memory file. Auto-detects the version and dispatches to the
/// correct parser.
pub fn parse_memory_file(content: &str) -> Result<Memory> {
    // Validate that frontmatter exists before version detection
    helpers::split_frontmatter(content)?;

    let version = detect_format_version(content);
    let parser = parser_for_version(version);
    parser.parse(content)
}

/// Write a memory in the latest format.
pub fn write_memory_file(memory: &Memory) -> Result<String> {
    let writer = latest_writer();
    writer.write(memory)
}
