//! Add a new memory to the store.

use crate::cli::output::OutputFormatter;
use crate::ops::{create_memory, parse_memory_type, parse_visibility, CreateParams};
use crate::storage::MemoryStore;
use crate::types::Provenance;
use anyhow::Result;
use std::path::Path;

/// Parameters for the add command.
pub struct AddParams {
    pub type_str: String,
    pub content: String,
    pub summary: Option<String>,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: f64,
    pub confidence: f64,
    pub details: Option<String>,
    pub visibility_str: String,
}

/// Add a new memory to the store.
///
/// Creates a new memory with the specified parameters, automatically generating
/// a summary if not provided and defaulting the physical scope to "/" if empty.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `params` - Memory creation parameters
/// * `formatter` - Output formatter for success/error messages
pub fn run_add(dir: &Path, params: AddParams, formatter: &OutputFormatter) -> Result<()> {
    let type_ = parse_memory_type(&params.type_str)?;
    let visibility = parse_visibility(&params.visibility_str)?;

    // Open or initialize store
    let store = match MemoryStore::open(dir) {
        Ok(s) => s,
        Err(_) => MemoryStore::init(dir)?,
    };

    let result = create_memory(
        &store,
        CreateParams {
            type_,
            content: params.content,
            summary: params.summary,
            physical: params.physical,
            logical: params.logical,
            tags: params.tags,
            criticality: params.criticality,
            confidence: params.confidence,
            details: params.details,
            visibility,
            provenance: Provenance::human(),
        },
    )?;

    formatter.print_success(&format!("Created memory {}", result.id));
    Ok(())
}
