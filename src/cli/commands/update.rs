//! Update an existing memory.

use crate::cli::output::OutputFormatter;
use crate::ops::{parse_memory_type, parse_status, parse_visibility};
use crate::ops::{update_memory, UpdateParams as OpsUpdateParams};
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// Parameters for the update command.
pub struct UpdateParams {
    pub id: String,
    pub type_: Option<String>,
    pub content: Option<String>,
    pub summary: Option<String>,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: Option<f64>,
    pub confidence: Option<f64>,
    pub details: Option<String>,
    pub visibility: Option<String>,
    pub status: Option<String>,
}

/// Update an existing memory.
///
/// Only the fields provided in params will be updated; others remain unchanged.
/// Automatically updates the `updated_at` timestamp.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `params` - Update parameters (only non-None fields are updated)
/// * `formatter` - Output formatter for success/error messages
pub fn run_update(dir: &Path, params: UpdateParams, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;

    let type_ = params.type_.map(|s| parse_memory_type(&s)).transpose()?;
    let visibility = params
        .visibility
        .map(|s| parse_visibility(&s))
        .transpose()?;
    let status = params.status.map(|s| parse_status(&s)).transpose()?;

    let physical = if params.physical.is_empty() {
        None
    } else {
        Some(params.physical)
    };

    let logical = if params.logical.is_empty() {
        None
    } else {
        Some(params.logical)
    };

    let tags = if params.tags.is_empty() {
        None
    } else {
        Some(params.tags)
    };

    update_memory(
        &store,
        &params.id,
        OpsUpdateParams {
            type_,
            content: params.content,
            summary: params.summary,
            physical,
            logical,
            tags,
            criticality: params.criticality,
            confidence: params.confidence,
            details: params.details,
            visibility,
            status,
        },
    )?;

    formatter.print_success(&format!("Updated memory {}", params.id));
    Ok(())
}
