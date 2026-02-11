//! Delete memory operation.

use crate::storage::MemoryStore;
use anyhow::Result;

/// Delete a memory by ID.
pub async fn delete_memory(store: &MemoryStore, id: &str) -> Result<bool> {
    store.delete(id).await?;
    Ok(true)
}
