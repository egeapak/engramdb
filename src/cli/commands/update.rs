use anyhow::{Result, bail};
use std::path::Path;
use crate::storage::MemoryStore;
use crate::types::{MemoryType, MemoryUpdate, Status, Visibility};
use crate::cli::output::OutputFormatter;

pub fn run_update(
    dir: &Path,
    id: &str,
    type_: Option<String>,
    content: Option<String>,
    summary: Option<String>,
    physical: Vec<String>,
    logical: Vec<String>,
    tags: Vec<String>,
    criticality: Option<f64>,
    confidence: Option<f64>,
    details: Option<String>,
    visibility: Option<String>,
    status: Option<String>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;

    // Build update
    let mut update = MemoryUpdate::new();

    if let Some(type_str) = type_ {
        update.type_ = Some(parse_memory_type(&type_str)?);
    }

    update.content = content;
    update.summary = summary;
    update.details = details;

    if !physical.is_empty() {
        update.physical = Some(physical);
    }

    if !logical.is_empty() {
        update.logical = Some(logical);
    }

    if !tags.is_empty() {
        update.tags = Some(tags);
    }

    update.criticality = criticality;
    update.confidence = confidence;

    if let Some(vis_str) = visibility {
        update.visibility = Some(parse_visibility(&vis_str)?);
    }

    if let Some(status_str) = status {
        update.status = Some(parse_status(&status_str)?);
    }

    // Apply update
    store.update(id, update)?;

    formatter.print_success(&format!("Updated memory {}", id));
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

fn parse_status(s: &str) -> Result<Status> {
    match s.to_lowercase().as_str() {
        "active" => Ok(Status::Active),
        "needsreview" | "needs-review" | "needs_review" => Ok(Status::NeedsReview),
        "challenged" => Ok(Status::Challenged),
        _ => bail!("Invalid status: {}. Valid values: active, needsreview, challenged", s),
    }
}
