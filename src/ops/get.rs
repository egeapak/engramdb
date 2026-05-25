//! Get single memory operation.

use crate::storage::MemoryStore;
use crate::types::Memory;
use anyhow::Result;

/// Get a memory by ID (supports prefix matching).
pub async fn get_memory(store: &MemoryStore, id: &str) -> Result<Memory> {
    let memory = store.get(id).await?;
    Ok(memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{MemoryType, Provenance};
    use tempfile::TempDir;

    #[tokio::test]
    async fn get_memory_propagates_not_found() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let result = get_memory(&store, "nope-xxx").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_memory_returns_created() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mem = Memory::new(
            MemoryType::Decision,
            "summary",
            "content",
            Provenance::human(),
        );
        let id = mem.id.clone();
        store.create(&mem).await.unwrap();

        let loaded = get_memory(&store, &id).await.unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.summary, "summary");
    }
}
