//! Update memory operation.

use crate::storage::MemoryStore;
use crate::types::{MemoryType, MemoryUpdate, Status, Visibility};
use anyhow::Result;

/// Parameters for updating a memory.
///
/// All fields are optional; only provided fields will be updated.
pub struct UpdateParams {
    pub type_: Option<MemoryType>,
    pub content: Option<String>,
    pub summary: Option<String>,
    pub physical: Option<Vec<String>>,
    pub logical: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub criticality: Option<f64>,
    pub confidence: Option<f64>,
    pub details: Option<String>,
    pub visibility: Option<Visibility>,
    pub status: Option<Status>,
}

/// Update an existing memory.
pub fn update_memory(store: &MemoryStore, id: &str, params: UpdateParams) -> Result<bool> {
    let mut update = MemoryUpdate::new();
    update.type_ = params.type_;
    update.content = params.content;
    update.summary = params.summary;
    update.details = params.details;
    update.physical = params.physical;
    update.logical = params.logical;
    update.tags = params.tags;
    update.criticality = params.criticality;
    update.confidence = params.confidence;
    update.visibility = params.visibility;
    update.status = params.status;

    store.update(id, update)?;
    Ok(true)
}
