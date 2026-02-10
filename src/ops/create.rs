//! Memory creation operation.

use crate::storage::MemoryStore;
use crate::types::{Memory, MemoryType, Provenance, Visibility};
use anyhow::Result;

/// Parameters for creating a new memory.
pub struct CreateParams {
    pub type_: MemoryType,
    pub content: String,
    pub summary: Option<String>,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: f64,
    pub confidence: f64,
    pub details: Option<String>,
    pub visibility: Visibility,
    pub provenance: Provenance,
}

/// Result of a create operation.
pub struct CreateResult {
    pub id: String,
    pub summary: String,
}

/// Create a new memory in the store.
pub fn create_memory(store: &MemoryStore, params: CreateParams) -> Result<CreateResult> {
    // Generate summary if not provided (truncate content to 100 chars)
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

    // Build memory
    let mut memory = Memory::new(params.type_, &summary, &params.content, params.provenance);
    memory.physical = physical;
    memory.logical = params.logical;
    memory.tags = params.tags;
    memory.criticality = params.criticality;
    memory.confidence = params.confidence;
    memory.details = params.details;
    memory.visibility = params.visibility;

    let id = store.create(&memory)?;

    Ok(CreateResult {
        id,
        summary: memory.summary,
    })
}
