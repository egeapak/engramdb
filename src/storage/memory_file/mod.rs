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

use super::error::{Result, StorageError};
use crate::types::Memory;

pub use v1::{V1Parser, V1Writer};
pub use v2::{V2Parser, V2Writer};

/// Current memory file format version.
pub const CURRENT_FORMAT_VERSION: u32 = 2;

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
    let mut parts = content.splitn(3, "---");
    parts.next(); // skip before first ---
    let frontmatter = parts.next()?.trim();

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
    let mut parts = content.splitn(3, "---");
    parts.next();
    if parts.next().is_none() {
        return Err(StorageError::InvalidFormat(
            "Missing frontmatter".to_string(),
        ));
    }

    let version = detect_format_version(content);
    let parser = parser_for_version(version);
    parser.parse(content)
}

/// Write a memory in the latest format.
pub fn write_memory_file(memory: &Memory) -> Result<String> {
    let writer = latest_writer();
    writer.write(memory)
}
