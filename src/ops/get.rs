//! Get single memory operation.

use crate::storage::MemoryStore;
use crate::types::Memory;
use anyhow::Result;

/// Get a memory by ID (supports prefix matching).
pub fn get_memory(store: &MemoryStore, id: &str) -> Result<Memory> {
    let memory = store.get(id)?;
    Ok(memory)
}
