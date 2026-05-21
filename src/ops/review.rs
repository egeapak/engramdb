//! List memories needing review.

use crate::storage::MemoryStore;
use crate::types::{Memory, MemoryType, Status};
use anyhow::Result;

/// Parameters for reviewing memories.
pub struct ReviewParams {
    pub scope: Option<String>,
    pub max_results: Option<usize>,
    pub type_filter: Option<MemoryType>,
    pub challenged_only: bool,
    pub stale_only: bool,
}

/// List memories that need review (status = NeedsReview or Challenged).
///
/// Returns memories sorted by criticality descending.
pub async fn review_memories(store: &MemoryStore, params: &ReviewParams) -> Result<Vec<Memory>> {
    let entries = store.list_summary().await?;

    // Filter candidates at the index level, then batch-load
    let candidate_ids: Vec<String> = entries
        .iter()
        .filter(|e| e.status == Status::NeedsReview || e.status == Status::Challenged)
        .filter(|e| {
            params
                .scope
                .as_ref()
                .is_none_or(|s| e.logical.iter().any(|l| l == s))
        })
        .map(|e| e.id.clone())
        .collect();

    let loaded = store.get_batch(&candidate_ids).await?;
    let mut memories: Vec<Memory> = loaded.into_iter().map(|(_, m)| m).collect();

    // Apply type filter
    if let Some(type_filter) = params.type_filter {
        memories.retain(|m| m.type_ == type_filter);
    }

    // Apply status filters
    if params.challenged_only {
        memories.retain(|m| matches!(m.status, Status::Challenged));
    } else if params.stale_only {
        memories.retain(|m| matches!(m.status, Status::NeedsReview));
    }

    // Sort by criticality descending
    memories.sort_by(|a, b| {
        b.criticality
            .partial_cmp(&a.criticality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(max) = params.max_results {
        memories.truncate(max);
    }

    Ok(memories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::Provenance;
    use tempfile::TempDir;

    async fn init_store(dir: &std::path::Path) -> MemoryStore {
        MemoryStore::init(dir, &InMemoryRegistry::new())
            .await
            .unwrap()
    }

    fn make(summary: &str, status: Status, criticality: f64) -> Memory {
        let mut m = Memory::new(MemoryType::Decision, summary, "body", Provenance::human());
        m.status = status;
        m.criticality = criticality;
        m
    }

    fn default_params() -> ReviewParams {
        ReviewParams {
            scope: None,
            max_results: None,
            type_filter: None,
            challenged_only: false,
            stale_only: false,
        }
    }

    async fn seed_mixed(store: &MemoryStore) -> (String, String, String) {
        let c = make("challenged item", Status::Challenged, 0.7);
        let n = make("needs review item", Status::NeedsReview, 0.9);
        let a = make("active item", Status::Active, 0.95);
        let ids = (c.id.clone(), n.id.clone(), a.id.clone());
        for m in [&c, &n, &a] {
            store.create(m).await.unwrap();
        }
        ids
    }

    #[tokio::test]
    async fn review_returns_only_review_statuses() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let (c, n, a) = seed_mixed(&store).await;

        let out = review_memories(&store, &default_params()).await.unwrap();
        let ids: std::collections::HashSet<_> = out.iter().map(|m| m.id.clone()).collect();
        assert!(ids.contains(&c));
        assert!(ids.contains(&n));
        // Active must not appear in review output (only NeedsReview/Challenged
        // do — see review.rs:25).
        assert!(!ids.contains(&a));
    }

    #[tokio::test]
    async fn review_challenged_only_excludes_needs_review() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let (c, n, _) = seed_mixed(&store).await;

        let mut params = default_params();
        params.challenged_only = true;

        let out = review_memories(&store, &params).await.unwrap();
        let ids: Vec<_> = out.iter().map(|m| m.id.clone()).collect();
        assert!(ids.contains(&c));
        assert!(!ids.contains(&n));
    }

    #[tokio::test]
    async fn review_stale_only_excludes_challenged() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let (c, n, _) = seed_mixed(&store).await;

        let mut params = default_params();
        params.stale_only = true;

        let out = review_memories(&store, &params).await.unwrap();
        let ids: Vec<_> = out.iter().map(|m| m.id.clone()).collect();
        assert!(ids.contains(&n));
        assert!(!ids.contains(&c));
    }

    /// `challenged_only` takes precedence over `stale_only` per the
    /// `else if` chain at review.rs:44-48. Lock in that precedence so a
    /// future refactor doesn't quietly swap the branches.
    #[tokio::test]
    async fn review_challenged_only_wins_over_stale_only() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let (c, n, _) = seed_mixed(&store).await;

        let mut params = default_params();
        params.challenged_only = true;
        params.stale_only = true;

        let out = review_memories(&store, &params).await.unwrap();
        let ids: Vec<_> = out.iter().map(|m| m.id.clone()).collect();
        assert!(ids.contains(&c));
        assert!(!ids.contains(&n));
    }

    #[tokio::test]
    async fn review_sorted_by_criticality_descending() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;

        // NeedsReview (high) → Challenged (low) → NeedsReview (mid).
        let lo = make("lo", Status::Challenged, 0.2);
        let mid = make("mid", Status::NeedsReview, 0.55);
        let hi = make("hi", Status::NeedsReview, 0.9);
        for m in [&lo, &mid, &hi] {
            store.create(m).await.unwrap();
        }

        let out = review_memories(&store, &default_params()).await.unwrap();
        assert_eq!(out.len(), 3);
        assert!(out[0].criticality >= out[1].criticality);
        assert!(out[1].criticality >= out[2].criticality);
        assert_eq!(out[0].id, hi.id);
        assert_eq!(out[2].id, lo.id);
    }

    #[tokio::test]
    async fn review_max_results_truncates() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        seed_mixed(&store).await;

        let mut params = default_params();
        params.max_results = Some(1);

        let out = review_memories(&store, &params).await.unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn review_type_filter_keeps_only_matching() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;

        let mut d = make("decision", Status::NeedsReview, 0.8);
        d.type_ = MemoryType::Decision;
        let mut h = make("hazard", Status::NeedsReview, 0.7);
        h.type_ = MemoryType::Hazard;
        store.create(&d).await.unwrap();
        store.create(&h).await.unwrap();

        let mut params = default_params();
        params.type_filter = Some(MemoryType::Hazard);

        let out = review_memories(&store, &params).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, h.id);
    }
}
