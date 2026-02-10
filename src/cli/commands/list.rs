//! List all memories with optional filtering.

use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use crate::types::{MemoryType, Status};
use anyhow::{bail, Result};
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

fn parse_memory_type(s: &str) -> Result<MemoryType> {
    match s.to_lowercase().as_str() {
        "decision" => Ok(MemoryType::Decision),
        "convention" => Ok(MemoryType::Convention),
        "hazard" => Ok(MemoryType::Hazard),
        "context" => Ok(MemoryType::Context),
        "intent" => Ok(MemoryType::Intent),
        "relationship" => Ok(MemoryType::Relationship),
        "debug" => Ok(MemoryType::Debug),
        "preference" => Ok(MemoryType::Preference),
        _ => bail!("Invalid memory type: {}. Valid types: decision, convention, hazard, context, intent, relationship, debug, preference", s),
    }
}

fn parse_status(s: &str) -> Result<Status> {
    match s.to_lowercase().as_str() {
        "active" => Ok(Status::Active),
        "needsreview" | "needs-review" | "needs_review" => Ok(Status::NeedsReview),
        "challenged" => Ok(Status::Challenged),
        _ => bail!(
            "Invalid status: {}. Valid values: active, needsreview, challenged",
            s
        ),
    }
}
