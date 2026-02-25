//! Resolve challenged or needs-review memories.

use crate::storage::MemoryStore;
use crate::types::{MemoryUpdate, Status};
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
pub async fn resolve_memory(store: &MemoryStore, params: ResolveParams) -> Result<ResolveResult> {
    match params.action {
        ResolveAction::Keep => {
            // Atomic update: set status, clear challenges, set verified_at
            let mut update = MemoryUpdate::new();
            update.status = Some(Status::Active);
            update.challenges = Some(vec![]);
            update.verified_at = Some(Utc::now());
            store.update(&params.id, update).await?;

            Ok(ResolveResult {
                resolved: true,
                action: "keep".to_string(),
            })
        }
        ResolveAction::Update => {
            // Atomic update: set content/summary, status, clear challenges, set verified_at
            let mut update = MemoryUpdate::new();
            update.content = params.updated_content;
            update.summary = params.updated_summary;
            update.status = Some(Status::Active);
            update.challenges = Some(vec![]);
            update.verified_at = Some(Utc::now());
            store.update(&params.id, update).await?;

            Ok(ResolveResult {
                resolved: true,
                action: "update".to_string(),
            })
        }
        ResolveAction::Delete => {
            store.delete(&params.id).await?;
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
    use crate::storage::InMemoryRegistry;
    use crate::types::{Challenge, Memory, MemoryType, Provenance, Status};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_resolve_keep() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

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
        store.create(&memory).await.unwrap();

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
        .await
        .unwrap();

        assert_eq!(result.action, "keep");
        assert!(result.resolved);

        // Verify memory was updated
        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.status, Status::Active);
        assert!(updated.challenges.is_empty());
        assert!(updated.verified_at.is_some());
    }

    #[tokio::test]
    async fn test_resolve_update() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

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
        store.create(&memory).await.unwrap();

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
        .await
        .unwrap();

        assert_eq!(result.action, "update");
        assert!(result.resolved);

        // Verify memory was updated
        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.status, Status::Active);
        assert!(updated.challenges.is_empty());
        assert!(updated.verified_at.is_some());
        assert_eq!(updated.content, "Updated content");
        assert_eq!(updated.summary, "Updated summary");
    }

    #[tokio::test]
    async fn test_resolve_delete() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Create a memory
        let memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );

        let id = memory.id.clone();
        store.create(&memory).await.unwrap();

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
        .await
        .unwrap();

        assert_eq!(result.action, "delete");
        assert!(result.resolved);

        // Verify memory was deleted
        assert!(store.get(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_resolve_keep_from_needs_review_status() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Needs review memory",
            "Content under review",
            Provenance::human(),
        );
        memory.status = Status::NeedsReview;
        memory.add_challenge(Challenge::new("Flagged for review"));

        let id = memory.id.clone();
        store.create(&memory).await.unwrap();

        let result = resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
            },
        )
        .await
        .unwrap();

        assert!(result.resolved);
        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.status, Status::Active);
        assert!(updated.verified_at.is_some());
        assert!(updated.challenges.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_update_only_content_preserves_summary() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original summary",
            "Original content",
            Provenance::human(),
        );
        memory.status = Status::Challenged;
        memory.add_challenge(Challenge::new("Challenge"));

        let id = memory.id.clone();
        store.create(&memory).await.unwrap();

        resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Update,
                updated_content: Some("New content only".to_string()),
                updated_summary: None,
            },
        )
        .await
        .unwrap();

        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.summary, "Original summary");
        assert_eq!(updated.content, "New content only");
    }

    #[tokio::test]
    async fn test_resolve_update_only_summary_preserves_content() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Original summary",
            "Original content",
            Provenance::human(),
        );
        memory.status = Status::Challenged;
        memory.add_challenge(Challenge::new("Challenge"));

        let id = memory.id.clone();
        store.create(&memory).await.unwrap();

        resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Update,
                updated_content: None,
                updated_summary: Some("New summary only".to_string()),
            },
        )
        .await
        .unwrap();

        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.summary, "New summary only");
        assert_eq!(updated.content, "Original content");
    }

    #[tokio::test]
    async fn test_resolve_keep_clears_multiple_challenges() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Multi-challenged",
            "Content",
            Provenance::human(),
        );
        memory.add_challenge(Challenge::new("Challenge 1"));
        memory.add_challenge(Challenge::new("Challenge 2"));
        memory.add_challenge(Challenge::new("Challenge 3"));
        assert_eq!(memory.challenges.len(), 3);

        let id = memory.id.clone();
        store.create(&memory).await.unwrap();

        resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
            },
        )
        .await
        .unwrap();

        let updated = store.get(&id).await.unwrap();
        assert!(updated.challenges.is_empty());
        assert_eq!(updated.status, Status::Active);
    }

    #[tokio::test]
    async fn test_resolve_keep_preserves_other_fields() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut memory = Memory::new(
            MemoryType::Hazard,
            "Important hazard",
            "Hazard content",
            Provenance::human(),
        );
        memory.status = Status::Challenged;
        memory.tags = vec!["safety".to_string(), "critical".to_string()];
        memory.logical = vec!["app.core".to_string()];
        memory.criticality = 0.9;
        memory.physical = vec!["/src/main.rs".to_string()];
        memory.add_challenge(Challenge::new("Test challenge"));

        let id = memory.id.clone();
        store.create(&memory).await.unwrap();

        resolve_memory(
            &store,
            ResolveParams {
                id: id.clone(),
                action: ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
            },
        )
        .await
        .unwrap();

        let updated = store.get(&id).await.unwrap();
        assert_eq!(updated.type_, MemoryType::Hazard);
        assert_eq!(
            updated.tags,
            vec!["safety".to_string(), "critical".to_string()]
        );
        assert_eq!(updated.logical, vec!["app.core".to_string()]);
        assert_eq!(updated.criticality, 0.9);
        assert_eq!(updated.physical, vec!["/src/main.rs".to_string()]);
    }

    #[tokio::test]
    async fn test_resolve_keep_nonexistent_id_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = resolve_memory(
            &store,
            ResolveParams {
                id: "nonexistent-id".to_string(),
                action: ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
            },
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_update_nonexistent_id_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = resolve_memory(
            &store,
            ResolveParams {
                id: "nonexistent-id".to_string(),
                action: ResolveAction::Update,
                updated_content: Some("New content".to_string()),
                updated_summary: Some("New summary".to_string()),
            },
        )
        .await;

        assert!(result.is_err());
    }
}
