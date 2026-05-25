//! Delete memory operation.

use crate::storage::MemoryStore;
use anyhow::Result;

/// Delete a memory by ID.
pub async fn delete_memory(store: &MemoryStore, id: &str) -> Result<bool> {
    store.delete(id).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    #[tokio::test]
    async fn delete_memory_propagates_not_found() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let result = delete_memory(&store, "does-not-exist").await;
        assert!(result.is_err(), "deleting unknown id must error");
    }

    #[tokio::test]
    async fn delete_memory_removes_existing() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mem = Memory::new(MemoryType::Decision, "S", "C", Provenance::human());
        let id = mem.id.clone();
        store.create(&mem).await.unwrap();

        assert!(delete_memory(&store, &id).await.unwrap());
        assert!(store.get(&id).await.is_err());
    }
}
