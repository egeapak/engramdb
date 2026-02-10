//! Add a new memory to the store.

use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use crate::types::{Memory, MemoryType, Provenance, Visibility};
use anyhow::{bail, Result};
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
    // Parse type
    let type_ = parse_memory_type(&params.type_str)?;

    // Parse visibility
    let visibility = parse_visibility(&params.visibility_str)?;

    // Generate summary if not provided
    let summary = params.summary.unwrap_or_else(|| {
        let max_len = 100;
        if params.content.len() <= max_len {
            params.content.clone()
        } else {
            format!("{}...", &params.content[..max_len])
        }
    });

    // Use default physical scope if empty
    let physical = if params.physical.is_empty() {
        vec!["/".to_string()]
    } else {
        params.physical
    };

    // Create provenance (CLI source is human)
    let provenance = Provenance::human();

    // Create memory
    let mut memory = Memory::new(type_, summary, &params.content, provenance);
    memory.physical = physical;
    memory.logical = params.logical;
    memory.tags = params.tags;
    memory.criticality = params.criticality;
    memory.confidence = params.confidence;
    memory.details = params.details;
    memory.visibility = visibility;

    // Open or initialize store
    let store = match MemoryStore::open(dir) {
        Ok(s) => s,
        Err(_) => MemoryStore::init(dir)?,
    };

    // Create memory
    let id = store.create(&memory)?;

    formatter.print_success(&format!("Created memory {}", id));
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

fn parse_visibility(s: &str) -> Result<Visibility> {
    match s.to_lowercase().as_str() {
        "shared" => Ok(Visibility::Shared),
        "personal" => Ok(Visibility::Personal),
        _ => bail!("Invalid visibility: {}. Valid values: shared, personal", s),
    }
}
