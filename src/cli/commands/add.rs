use anyhow::{Result, bail};
use std::path::Path;
use crate::storage::MemoryStore;
use crate::types::{Memory, MemoryType, Provenance, Visibility};
use crate::cli::output::OutputFormatter;

pub fn run_add(
    dir: &Path,
    type_str: &str,
    content: &str,
    summary: Option<String>,
    physical: Vec<String>,
    logical: Vec<String>,
    tags: Vec<String>,
    criticality: f64,
    confidence: f64,
    details: Option<String>,
    visibility_str: &str,
    formatter: &OutputFormatter,
) -> Result<()> {
    // Parse type
    let type_ = parse_memory_type(type_str)?;

    // Parse visibility
    let visibility = parse_visibility(visibility_str)?;

    // Generate summary if not provided
    let summary = summary.unwrap_or_else(|| {
        let max_len = 100;
        if content.len() <= max_len {
            content.to_string()
        } else {
            format!("{}...", &content[..max_len])
        }
    });

    // Use default physical scope if empty
    let physical = if physical.is_empty() {
        vec!["/".to_string()]
    } else {
        physical
    };

    // Create provenance (CLI source is human)
    let provenance = Provenance::human();

    // Create memory
    let mut memory = Memory::new(type_, summary, content, provenance);
    memory.physical = physical;
    memory.logical = logical;
    memory.tags = tags;
    memory.criticality = criticality;
    memory.confidence = confidence;
    memory.details = details;
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
