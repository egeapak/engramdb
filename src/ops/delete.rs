//! Delete memory operation.

use crate::storage::MemoryStore;
use anyhow::Result;

/// Delete a memory by ID (or unique prefix).
///
/// Records a `deleted` usage event from `surface`. The full id is resolved
/// before the delete, since afterwards nothing resolves it.
pub async fn delete_memory(
    store: &MemoryStore,
    id: &str,
    surface: super::usage::Surface,
) -> Result<bool> {
    let recording = super::usage::usage_enabled(store).await;
    let full = if recording {
        store.resolve_id(id).await.ok()
    } else {
        None
    };
    store.delete(id).await?;
    if let Some(full) = full {
        super::usage::record_resolved(
            store,
            &full,
            super::usage::UsageKind::Deleted,
            surface,
            None,
        );
    }
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

        let result = delete_memory(&store, "does-not-exist", crate::ops::Surface::Cli).await;
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

        assert!(delete_memory(&store, &id, crate::ops::Surface::Cli)
            .await
            .unwrap());
        assert!(store.get(&id).await.is_err());
    }
}
