//! Resolve challenged or needs-review memories.

use crate::storage::MemoryStore;
use crate::types::Status;
use anyhow::Result;
use chrono::Utc;

/// Action to take when resolving a memory.
pub enum ResolveAction {
    Keep,
    Update,
    Delete,
}

/// Parameters for resolving a memory.
pub struct ResolveParams {
    pub id: String,
    pub action: ResolveAction,
    pub updated_content: Option<String>,
    pub updated_summary: Option<String>,
}

/// Result of a resolve operation.
pub struct ResolveResult {
    pub resolved: bool,
    pub action: String,
}

/// Resolve a challenged or needs-review memory.
///
/// - Keep: Set status to Active, clear challenges, set verified_at to now
/// - Update: Same as keep, plus update content/summary
/// - Delete: Remove the memory entirely
pub fn resolve_memory(store: &MemoryStore, params: ResolveParams) -> Result<ResolveResult> {
    match params.action {
        ResolveAction::Keep => {
            // Get memory, modify it directly, then save it back
            let mut memory = store.get(&params.id)?;
            memory.status = Status::Active;
            memory.challenges.clear();
            memory.verified_at = Some(Utc::now());
            memory.mark_updated();

            // Delete and recreate to ensure challenges are cleared
            store.delete(&params.id)?;
            store.create(&memory)?;

            Ok(ResolveResult {
                resolved: true,
                action: "keep".to_string(),
            })
        }
        ResolveAction::Update => {
            // Get memory, modify it directly, then save it back
            let mut memory = store.get(&params.id)?;

            if let Some(content) = params.updated_content {
                memory.content = content;
            }
            if let Some(summary) = params.updated_summary {
                memory.summary = summary;
            }

            memory.status = Status::Active;
            memory.challenges.clear();
            memory.verified_at = Some(Utc::now());
            memory.mark_updated();

            // Delete and recreate to ensure challenges are cleared
            store.delete(&params.id)?;
            store.create(&memory)?;

            Ok(ResolveResult {
                resolved: true,
                action: "update".to_string(),
            })
        }
        ResolveAction::Delete => {
            store.delete(&params.id)?;
            Ok(ResolveResult {
                resolved: true,
                action: "delete".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Challenge, Memory, MemoryType, Provenance, Status};
    use tempfile::TempDir;

    #[test]
    fn test_resolve_keep() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

        // Create a challenged memory
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );
        memory.status = Status::Challenged;
        memory.add_challenge(Challenge::new("Test challenge"));

        let id = memory.id.clone();
        store.create(&memory).unwrap();

        // Resolve with Keep
        let result = resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
            },
        )
        .unwrap();

        assert_eq!(result.action, "keep");
        assert!(result.resolved);

        // Verify memory was updated
        let updated = store.get(&id).unwrap();
        assert_eq!(updated.status, Status::Active);
        assert!(updated.challenges.is_empty());
        assert!(updated.verified_at.is_some());
    }

    #[test]
    fn test_resolve_update() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

        // Create a challenged memory
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );
        memory.status = Status::Challenged;
        memory.add_challenge(Challenge::new("Test challenge"));

        let id = memory.id.clone();
        store.create(&memory).unwrap();

        // Resolve with Update
        let result = resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Update,
                updated_content: Some("Updated content".to_string()),
                updated_summary: Some("Updated summary".to_string()),
            },
        )
        .unwrap();

        assert_eq!(result.action, "update");
        assert!(result.resolved);

        // Verify memory was updated
        let updated = store.get(&id).unwrap();
        assert_eq!(updated.status, Status::Active);
        assert!(updated.challenges.is_empty());
        assert!(updated.verified_at.is_some());
        assert_eq!(updated.content, "Updated content");
        assert_eq!(updated.summary, "Updated summary");
    }

    #[test]
    fn test_resolve_delete() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

        // Create a memory
        let memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );

        let id = memory.id.clone();
        store.create(&memory).unwrap();

        // Resolve with Delete
        let result = resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Delete,
                updated_content: None,
                updated_summary: None,
            },
        )
        .unwrap();

        assert_eq!(result.action, "delete");
        assert!(result.resolved);

        // Verify memory was deleted
        assert!(store.get(&id).is_err());
    }
}
