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
pub fn compute_stats(store: &MemoryStore) -> Result<StoreStats> {
    let entries = store.list()?;
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
    use crate::types::{Memory, MemoryType, Provenance};
    use chrono::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_compute_stats_basic() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

        let mut mem1 = Memory::new(
            MemoryType::Decision,
            "Test 1",
            "Content 1",
            Provenance::human(),
        );
        mem1.logical = vec!["app.core".to_string()];
        store.create(&mem1).unwrap();

        let mut mem2 = Memory::new(
            MemoryType::Hazard,
            "Test 2",
            "Content 2",
            Provenance::agent("test"),
        );
        mem2.logical = vec!["app.core".to_string(), "app.utils".to_string()];
        store.create(&mem2).unwrap();

        let stats = compute_stats(&store).unwrap();
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

    #[test]
    fn test_compute_stats_with_expired() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

        let mut mem1 = Memory::new(MemoryType::Debug, "Expired", "Content", Provenance::human());
        mem1.expires_at = Some(Utc::now() - Duration::days(1));
        store.create(&mem1).unwrap();

        let mem2 = Memory::new(
            MemoryType::Decision,
            "Active",
            "Content",
            Provenance::human(),
        );
        store.create(&mem2).unwrap();

        let stats = compute_stats(&store).unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.expired, 1);
    }

    #[test]
    fn test_compute_stats_empty() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

        let stats = compute_stats(&store).unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.expired, 0);
        assert!(stats.oldest.is_none());
        assert!(stats.newest.is_none());
        assert_eq!(stats.avg_criticality, 0.0);
    }
}
