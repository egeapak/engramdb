//! List all memories with optional filtering.

use crate::cli::output::OutputFormatter;
use crate::ops::{parse_memory_type, parse_status};
use crate::storage::MemoryStore;
use crate::types::MemoryType;
use anyhow::Result;
use std::path::Path;

/// List all memories, optionally filtered by type, tags, or status.
///
/// Returns index entries (lightweight summaries) rather than full memory data.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `type_filter` - Filter by memory types (empty = no filter)
/// * `tags_filter` - Filter by tags, OR logic (empty = no filter)
/// * `status_filter` - Filter by status (None = no filter)
/// * `formatter` - Output formatter for displaying the list
pub fn run_list(
    dir: &Path,
    type_filter: Vec<String>,
    tags_filter: Vec<String>,
    status_filter: Option<String>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let mut entries = store.list()?;

    // Apply filters
    if !type_filter.is_empty() {
        let types: Vec<MemoryType> = type_filter
            .iter()
            .map(|s| parse_memory_type(s))
            .collect::<Result<Vec<_>>>()?;
        entries.retain(|e| types.contains(&e.type_));
    }

    if !tags_filter.is_empty() {
        entries.retain(|e| tags_filter.iter().any(|tag| e.tags.contains(tag)));
    }

    if let Some(status_str) = status_filter {
        let status = parse_status(&status_str)?;
        entries.retain(|e| e.status == status);
    }

    formatter.print_memory_list(&entries);
    Ok(())
}
