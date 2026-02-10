//! Delete memory operation.

use crate::storage::MemoryStore;
use anyhow::Result;

/// Delete a memory by ID.
pub fn delete_memory(store: &MemoryStore, id: &str) -> Result<bool> {
    store.delete(id)?;
    Ok(true)
}
