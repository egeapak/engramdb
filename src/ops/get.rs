//! Get single memory operation.

use crate::storage::MemoryStore;
use crate::types::Memory;
use anyhow::Result;

/// Get a memory by ID (supports prefix matching).
pub async fn get_memory(store: &MemoryStore, id: &str) -> Result<Memory> {
    let memory = store.get(id).await?;
    Ok(memory)
}
