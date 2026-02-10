use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use crate::types::{MemoryType, MemoryUpdate, Status, Visibility};
use anyhow::{bail, Result};
use std::path::Path;

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

pub fn run_update(dir: &Path, params: UpdateParams, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;

    // Build update
    let mut update = MemoryUpdate::new();

    if let Some(type_str) = params.type_ {
        update.type_ = Some(parse_memory_type(&type_str)?);
    }

    update.content = params.content;
    update.summary = params.summary;
    update.details = params.details;

    if !params.physical.is_empty() {
        update.physical = Some(params.physical);
    }

    if !params.logical.is_empty() {
        update.logical = Some(params.logical);
    }

    if !params.tags.is_empty() {
        update.tags = Some(params.tags);
    }

    update.criticality = params.criticality;
    update.confidence = params.confidence;

    if let Some(vis_str) = params.visibility {
        update.visibility = Some(parse_visibility(&vis_str)?);
    }

    if let Some(status_str) = params.status {
        update.status = Some(parse_status(&status_str)?);
    }

    // Apply update
    store.update(&params.id, update)?;

    formatter.print_success(&format!("Updated memory {}", params.id));
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
        _ => bail!(
            "Invalid status: {}. Valid values: active, needsreview, challenged",
            s
        ),
    }
}
