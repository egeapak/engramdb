//! Compute store statistics operation.

use crate::storage::MemoryStore;
use crate::types::{MemoryType, Status};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Statistics about the memory store.
pub struct StoreStats {
    pub total: usize,
    pub by_type: Vec<(MemoryType, usize)>,
    pub by_status: Vec<(Status, usize)>,
    pub by_scope: Vec<(String, usize)>,
    pub expired: usize,
    pub oldest: Option<DateTime<Utc>>,
    pub newest: Option<DateTime<Utc>>,
    pub avg_criticality: f64,
}

/// Compute statistics for the memory store.
pub async fn compute_stats(store: &MemoryStore) -> Result<StoreStats> {
    let entries = store.list_summary().await?;
    let total = entries.len();

    let mut type_counts: HashMap<MemoryType, usize> = HashMap::new();
    for entry in &entries {
        *type_counts.entry(entry.type_).or_insert(0) += 1;
    }
    let mut by_type: Vec<_> = type_counts.into_iter().collect();
    by_type.sort_by_key(|(t, _)| format!("{:?}", t));

    let mut status_counts: HashMap<Status, usize> = HashMap::new();
    for entry in &entries {
        *status_counts.entry(entry.status).or_insert(0) += 1;
    }
    let mut by_status: Vec<_> = status_counts.into_iter().collect();
    by_status.sort_by_key(|(s, _)| format!("{:?}", s));

    // Count memories per logical scope
    let mut scope_counts: HashMap<String, usize> = HashMap::new();
    for entry in &entries {
        for scope in &entry.logical {
            *scope_counts.entry(scope.clone()).or_insert(0) += 1;
        }
    }
    let mut by_scope: Vec<_> = scope_counts.into_iter().collect();
    by_scope.sort_by(|(a, _), (b, _)| a.cmp(b));

    // Count expired memories
    let now = Utc::now();
    let expired = entries
        .iter()
        .filter(|e| e.expires_at.map(|exp| exp < now).unwrap_or(false))
        .count();

    // Find oldest and newest
    let oldest = entries.iter().map(|e| e.created_at).min();
    let newest = entries.iter().map(|e| e.created_at).max();

    let avg_criticality = if total > 0 {
        entries.iter().map(|e| e.criticality).sum::<f64>() / total as f64
    } else {
        0.0
    };

    Ok(StoreStats {
        total,
        by_type,
        by_status,
        by_scope,
        expired,
        oldest,
        newest,
        avg_criticality,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance, Status};
    use chrono::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_compute_stats_basic() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut mem1 = Memory::new(
            MemoryType::Decision,
            "Test 1",
            "Content 1",
            Provenance::human(),
        );
        mem1.logical = vec!["app.core".to_string()];
        store.create(&mem1).await.unwrap();

        let mut mem2 = Memory::new(
            MemoryType::Hazard,
            "Test 2",
            "Content 2",
            Provenance::agent("test"),
        );
        mem2.logical = vec!["app.core".to_string(), "app.utils".to_string()];
        store.create(&mem2).await.unwrap();

        let stats = compute_stats(&store).await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(
            stats
                .by_scope
                .iter()
                .find(|(s, _)| s == "app.core")
                .unwrap()
                .1,
            2
        );
        assert_eq!(
            stats
                .by_scope
                .iter()
                .find(|(s, _)| s == "app.utils")
                .unwrap()
                .1,
            1
        );
        assert_eq!(stats.expired, 0);
        assert!(stats.oldest.is_some());
        assert!(stats.newest.is_some());
    }

    #[tokio::test]
    async fn test_compute_stats_with_expired() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut mem1 = Memory::new(MemoryType::Debug, "Expired", "Content", Provenance::human());
        mem1.expires_at = Some(Utc::now() - Duration::days(1));
        store.create(&mem1).await.unwrap();

        let mem2 = Memory::new(
            MemoryType::Decision,
            "Active",
            "Content",
            Provenance::human(),
        );
        store.create(&mem2).await.unwrap();

        let stats = compute_stats(&store).await.unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.expired, 1);
    }

    #[tokio::test]
    async fn test_compute_stats_empty() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let stats = compute_stats(&store).await.unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.expired, 0);
        assert!(stats.oldest.is_none());
        assert!(stats.newest.is_none());
        assert_eq!(stats.avg_criticality, 0.0);
    }

    #[tokio::test]
    async fn test_compute_stats_by_type_multiple_types() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // 2 Decision + 1 Hazard + 3 Context
        for _ in 0..2 {
            store
                .create(&Memory::new(
                    MemoryType::Decision,
                    "Decision",
                    "Content",
                    Provenance::human(),
                ))
                .await
                .unwrap();
        }
        store
            .create(&Memory::new(
                MemoryType::Hazard,
                "Hazard",
                "Content",
                Provenance::human(),
            ))
            .await
            .unwrap();
        for _ in 0..3 {
            store
                .create(&Memory::new(
                    MemoryType::Context,
                    "Context",
                    "Content",
                    Provenance::human(),
                ))
                .await
                .unwrap();
        }

        let stats = compute_stats(&store).await.unwrap();
        assert_eq!(stats.total, 6);

        let decision_count = stats
            .by_type
            .iter()
            .find(|(t, _)| *t == MemoryType::Decision)
            .unwrap()
            .1;
        assert_eq!(decision_count, 2);

        let hazard_count = stats
            .by_type
            .iter()
            .find(|(t, _)| *t == MemoryType::Hazard)
            .unwrap()
            .1;
        assert_eq!(hazard_count, 1);

        let context_count = stats
            .by_type
            .iter()
            .find(|(t, _)| *t == MemoryType::Context)
            .unwrap()
            .1;
        assert_eq!(context_count, 3);
    }

    #[tokio::test]
    async fn test_compute_stats_by_status_mixed() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // 2 Active
        store
            .create(&Memory::new(
                MemoryType::Decision,
                "Active 1",
                "Content",
                Provenance::human(),
            ))
            .await
            .unwrap();
        store
            .create(&Memory::new(
                MemoryType::Decision,
                "Active 2",
                "Content",
                Provenance::human(),
            ))
            .await
            .unwrap();

        // 1 NeedsReview
        let mut needs_review = Memory::new(
            MemoryType::Decision,
            "Needs review",
            "Content",
            Provenance::human(),
        );
        needs_review.status = Status::NeedsReview;
        store.create(&needs_review).await.unwrap();

        // 1 Challenged
        let mut challenged = Memory::new(
            MemoryType::Decision,
            "Challenged",
            "Content",
            Provenance::human(),
        );
        challenged.status = Status::Challenged;
        store.create(&challenged).await.unwrap();

        let stats = compute_stats(&store).await.unwrap();
        assert_eq!(stats.total, 4);

        let active_count = stats
            .by_status
            .iter()
            .find(|(s, _)| *s == Status::Active)
            .unwrap()
            .1;
        assert_eq!(active_count, 2);

        let needs_review_count = stats
            .by_status
            .iter()
            .find(|(s, _)| *s == Status::NeedsReview)
            .unwrap()
            .1;
        assert_eq!(needs_review_count, 1);

        let challenged_count = stats
            .by_status
            .iter()
            .find(|(s, _)| *s == Status::Challenged)
            .unwrap()
            .1;
        assert_eq!(challenged_count, 1);
    }

    #[tokio::test]
    async fn test_compute_stats_by_scope_no_logical_scopes() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Memories with empty logical scopes (default)
        store
            .create(&Memory::new(
                MemoryType::Decision,
                "Test 1",
                "Content",
                Provenance::human(),
            ))
            .await
            .unwrap();
        store
            .create(&Memory::new(
                MemoryType::Context,
                "Test 2",
                "Content",
                Provenance::human(),
            ))
            .await
            .unwrap();

        let stats = compute_stats(&store).await.unwrap();
        assert_eq!(stats.total, 2);
        assert!(stats.by_scope.is_empty());
    }

    #[tokio::test]
    async fn test_compute_stats_avg_criticality_calculation() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let criticalities = [0.2, 0.5, 0.8];
        for &crit in &criticalities {
            let mut mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
            mem.criticality = crit;
            store.create(&mem).await.unwrap();
        }

        let stats = compute_stats(&store).await.unwrap();
        assert!((stats.avg_criticality - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_compute_stats_oldest_newest_ordering() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut mem1 = Memory::new(
            MemoryType::Decision,
            "Oldest",
            "Content",
            Provenance::human(),
        );
        mem1.created_at = Utc::now() - Duration::hours(2);
        store.create(&mem1).await.unwrap();

        let mut mem2 = Memory::new(
            MemoryType::Decision,
            "Middle",
            "Content",
            Provenance::human(),
        );
        mem2.created_at = Utc::now() - Duration::hours(1);
        store.create(&mem2).await.unwrap();

        let mem3 = Memory::new(
            MemoryType::Decision,
            "Newest",
            "Content",
            Provenance::human(),
        );
        store.create(&mem3).await.unwrap();

        let stats = compute_stats(&store).await.unwrap();
        let oldest = stats.oldest.unwrap();
        let newest = stats.newest.unwrap();
        assert!(oldest < newest);
        assert_eq!(oldest, mem1.created_at);
    }
}
