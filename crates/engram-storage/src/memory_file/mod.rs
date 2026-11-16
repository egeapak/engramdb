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
mod yaml;

#[cfg(test)]
mod tests;

use super::error::Result;
use engram_types::Memory;

pub use helpers::is_crlf_terminated;
pub use v1::{V1Parser, V1Writer};
pub use v2::{V2Parser, V2Writer};

/// Current memory file format version.
pub const CURRENT_FORMAT_VERSION: u32 = 2;

/// Reject frontmatter `id` values that could escape the memories directory
/// when composed into a filename (`memory_filename` → `dir.join(...)`).
///
/// Server-generated IDs are UUIDv7, but memory files can arrive with a
/// cloned repo, so the parser is an untrusted-input boundary. Deliberately
/// shape-loose (legacy stores may carry non-UUID ids): only path-hostile
/// content is rejected.
pub(crate) fn validate_id_shape(id: &str) -> Result<()> {
    let hostile = id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.chars().any(|c| c.is_control());
    if hostile {
        return Err(super::error::StorageError::Validation(format!(
            "memory id {id:?} is empty or contains path-hostile characters"
        )));
    }
    Ok(())
}

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
        // Truncate to at most 50 bytes WITHOUT slicing mid-character:
        // is_alphanumeric() keeps multibyte chars (CJK, accented Latin), so
        // byte offset 50 may not be a char boundary and `&trimmed[..50]`
        // would panic on e.g. a 17-char CJK title.
        let mut end = 50;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = &trimmed[..end];
        // Break at last hyphen boundary when there is one
        match truncated.rfind('-') {
            Some(pos) => truncated[..pos].to_string(),
            None => truncated.to_string(),
        }
    }
}

/// Generate a memory filename from the memory's title and ID.
///
/// If the memory has a title **and a UUID-shaped id**, the filename is
/// `<slug>_<uuid>.md`. Otherwise it is `<id>.md`.
///
/// The slug is only added to a UUID-shaped id because that shape is how
/// [`extract_id_from_stem`] finds where the slug ends. A hand-authored id
/// (`auth-decision`) with a slug in front (`my-title_auth-decision`) read
/// back as the id `my-title_auth-decision`, so the memory became unreachable
/// by its own id the moment it was given a title.
///
/// One shape stays ambiguous by construction and is not supported: a
/// non-UUID id that itself ends in `_<36 chars with 4 hyphens>` reads back
/// as that tail.
pub fn memory_filename(memory: &Memory) -> String {
    if let Some(ref title) = memory.title {
        let slug = slugify(title);
        if slug.is_empty() || !is_uuid_shaped(&memory.id) {
            format!("{}.md", memory.id)
        } else {
            format!("{}_{}.md", slug, memory.id)
        }
    } else {
        format!("{}.md", memory.id)
    }
}

/// The shape [`extract_id_from_stem`] recognises as an id after a slug:
/// 36 bytes with exactly 4 hyphens (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`)
/// and no underscore — the slug separator is the LAST underscore of the
/// stem, so an id with one of its own could not be told from its slug.
pub fn is_uuid_shaped(id: &str) -> bool {
    id.len() == 36 && id.chars().filter(|c| *c == '-').count() == 4 && !id.contains('_')
}

/// Extract the memory ID (UUID) from a file stem.
///
/// Handles both old format (`<uuid>`) and new format (`<slug>_<uuid>`).
/// UUID v7 is always 36 characters: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
pub fn extract_id_from_stem(stem: &str) -> &str {
    if let Some(pos) = stem.rfind('_') {
        let candidate = &stem[pos + 1..];
        if is_uuid_shaped(candidate) {
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

    /// [`Self::parse`], with a source for a timestamp the file does not
    /// record. Formats that always record their timestamps ignore it.
    fn parse_with_fallback(
        &self,
        content: &str,
        _fallback_created: &dyn Fn() -> Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Memory> {
        self.parse(content)
    }

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
///
/// A file with no `Created` field gets the current time, so two parses of
/// such a file disagree. That is right for a pure caller with nothing better
/// to offer; anything that read the content from a file should call
/// [`parse_memory_file_at`] or [`parse_memory_file_from`] instead.
pub fn parse_memory_file(content: &str) -> Result<Memory> {
    parse_memory_file_with(content, &|| None)
}

/// [`parse_memory_file`] with a fixed fallback for a missing `Created`
/// timestamp (and for `Updated`, which falls back to `Created`).
pub fn parse_memory_file_at(
    content: &str,
    fallback_created: chrono::DateTime<chrono::Utc>,
) -> Result<Memory> {
    parse_memory_file_with(content, &|| Some(fallback_created))
}

/// [`parse_memory_file`] for content read from `path`: a missing `Created`
/// falls back to the file's modification time, so the same file parses to
/// the same timestamps every time. The file is only `stat`ed when the field
/// is actually missing.
pub fn parse_memory_file_from(content: &str, path: &std::path::Path) -> Result<Memory> {
    parse_memory_file_with(content, &|| {
        std::fs::metadata(path)
            .ok()?
            .modified()
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from)
    })
}

fn parse_memory_file_with(
    content: &str,
    fallback_created: &dyn Fn() -> Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Memory> {
    // Validate that frontmatter exists before version detection
    helpers::split_frontmatter(content)?;

    let version = detect_format_version(content);
    let parser = parser_for_version(version);
    parser.parse_with_fallback(content, fallback_created)
}

/// Best-effort recovery of the memory id of a file that does not parse.
///
/// Used by `reindex`, which must not let a file it cannot read (a merge
/// conflict in the hidden block is the common case) drop that memory's index
/// row and vectors: it needs the id to keep them. Tries the frontmatter's
/// `id` line first (YAML `id: …` or TOML `id = "…"`, inside the leading `---`
/// fence), then the filename stem. `None` when neither yields a UUID.
pub fn recover_id(content: &str, path: &std::path::Path) -> Option<String> {
    fn as_uuid(s: &str) -> Option<String> {
        let s = s.trim().trim_matches(|c| c == '"' || c == '\'');
        uuid::Uuid::parse_str(s)
            .ok()
            .map(|_| s.to_ascii_lowercase())
    }
    let mut lines = content.lines();
    if lines.next().map(str::trim) == Some("---") {
        for line in lines {
            let line = line.trim();
            if line == "---" {
                break;
            }
            if let Some(rest) = line
                .strip_prefix("id:")
                .or_else(|| line.strip_prefix("id ="))
                .or_else(|| line.strip_prefix("id="))
            {
                if let Some(id) = as_uuid(rest) {
                    return Some(id);
                }
            }
        }
    }
    let stem = path.file_stem()?.to_str()?;
    as_uuid(extract_id_from_stem(stem))
}

/// Write a memory in the latest format.
pub fn write_memory_file(memory: &Memory) -> Result<String> {
    let writer = latest_writer();
    writer.write(memory)
}
