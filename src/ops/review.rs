//! List memories needing review.
//!
//! A memory is a review candidate when it is explicitly flagged
//! ([`Status::NeedsReview`] / [`Status::Challenged`]) or — when the caller opts
//! in via [`ReviewParams::stale_after_days`] — when it is still active but has
//! not been touched (updated or resolved) in longer than the recency window.
//! The recency arm is a *review* trigger, not a TTL: it never deletes or hides a
//! memory, it only suggests re-verifying it. `updated_at` is the anchor because
//! every edit and every `resolve` (keep/update) bumps it, so routinely-touched
//! memories never go stale, while a memory nobody has revisited eventually
//! surfaces for a human/agent to confirm or retire.

use crate::storage::MemoryStore;
use crate::types::{Memory, MemoryType, Status};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

/// Parameters for reviewing memories.
pub struct ReviewParams {
    pub scope: Option<String>,
    pub max_results: Option<usize>,
    pub type_filter: Option<MemoryType>,
    pub challenged_only: bool,
    pub stale_only: bool,
    /// When set, *also* surface active memories whose last update is older than
    /// this many days (the recency trigger). `None` keeps the classic behavior
    /// of listing only flagged (`NeedsReview`/`Challenged`) memories.
    pub stale_after_days: Option<u64>,
}

/// Resolve a "N days ago" cutoff instant, guarding against overflow and absurd
/// inputs. Returns `None` when the trigger is disabled — either the caller
/// passed no window, passed `0` (a 0-day window would flag every active memory,
/// so it is treated as "off" rather than "all"), or the window overflows.
fn recency_cutoff(now: DateTime<Utc>, days: Option<u64>) -> Option<DateTime<Utc>> {
    let days = days?;
    if days == 0 {
        return None;
    }
    let signed = i64::try_from(days).ok()?;
    Duration::try_days(signed).map(|window| now - window)
}

/// Whether an index entry with the given `status`/`updated_at` is a review
/// candidate. Flagged memories always qualify; active memories qualify only when
/// the recency trigger is armed and their last update predates the cutoff.
fn is_review_candidate(
    status: Status,
    updated_at: DateTime<Utc>,
    recency_cutoff: Option<DateTime<Utc>>,
) -> bool {
    match status {
        Status::NeedsReview | Status::Challenged => true,
        Status::Active => recency_cutoff.is_some_and(|cutoff| updated_at < cutoff),
    }
}

/// Count active memories that have gone stale under the recency trigger.
///
/// Cheap and index-only (no `.md` loads), so it is safe to call on hot paths
/// like the MCP session-end hint. Returns 0 when the trigger is disabled.
pub async fn count_recency_stale(store: &MemoryStore, recency_days: Option<u64>) -> Result<usize> {
    let now = Utc::now();
    let Some(cutoff) = recency_cutoff(now, recency_days) else {
        return Ok(0);
    };
    let entries = store.list_filterable().await?;
    Ok(entries
        .iter()
        .filter(|e| e.invalidated_at.is_none_or(|t| t > now))
        .filter(|e| {
            is_review_candidate(e.status, e.updated_at, Some(cutoff)) && e.status == Status::Active
        })
        .count())
}

/// List memories that need review.
///
/// Always includes flagged memories (`NeedsReview`/`Challenged`); when
/// [`ReviewParams::stale_after_days`] is set, also includes active memories that
/// are stale under the recency trigger. Returns memories sorted by criticality
/// descending.
pub async fn review_memories(store: &MemoryStore, params: &ReviewParams) -> Result<Vec<Memory>> {
    let now = Utc::now();
    let cutoff = recency_cutoff(now, params.stale_after_days);
    let entries = store.list_filterable().await?;

    // Filter candidates at the index level, then batch-load.
    //
    // Invalidated memories (closed validity windows, §2.4) are excluded from
    // BOTH arms: they are resolved history, not live knowledge. Without this,
    // a Challenged memory resolved via `invalidate` (or superseded by a new
    // write) would reappear in review forever, and the recency trigger would
    // nominate closed-window history for re-verification. A future-dated
    // `invalidated_at` is still valid and stays reviewable.
    let candidate_ids: Vec<String> = entries
        .iter()
        .filter(|e| e.invalidated_at.is_none_or(|t| t > now))
        .filter(|e| is_review_candidate(e.status, e.updated_at, cutoff))
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

    // Apply status filters. These narrow to a single flagged status, so they
    // are mutually exclusive with the recency arm (whose candidates are Active):
    // passing `challenged_only`/`stale_only` alongside `stale_after_days` keeps
    // only the flagged subset, which is the intended "narrow to X" semantics.
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
            stale_after_days: None,
        }
    }

    /// An active memory whose `updated_at` is backdated by `age_days`.
    fn make_aged(summary: &str, criticality: f64, age_days: i64) -> Memory {
        let mut m = make(summary, Status::Active, criticality);
        m.updated_at = Utc::now() - Duration::days(age_days);
        m
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

    // --- Recency trigger -------------------------------------------------

    #[tokio::test]
    async fn recency_disabled_excludes_old_active() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        store.create(&make_aged("ancient", 0.8, 400)).await.unwrap();

        // No `stale_after_days` ⇒ classic behavior, active memories never listed.
        let out = review_memories(&store, &default_params()).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn recency_surfaces_old_active_but_not_fresh() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let old = make_aged("stale", 0.8, 200);
        let fresh = make_aged("fresh", 0.9, 5);
        store.create(&old).await.unwrap();
        store.create(&fresh).await.unwrap();

        let mut params = default_params();
        params.stale_after_days = Some(90);

        let out = review_memories(&store, &params).await.unwrap();
        let ids: std::collections::HashSet<_> = out.iter().map(|m| m.id.clone()).collect();
        assert!(
            ids.contains(&old.id),
            "200-day-old active memory should surface"
        );
        assert!(
            !ids.contains(&fresh.id),
            "5-day-old active memory is within the window"
        );
    }

    #[tokio::test]
    async fn recency_boundary_is_strict() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        // 100 days old, window 90 ⇒ stale; window 120 ⇒ within window.
        let m = make_aged("edge", 0.5, 100);
        store.create(&m).await.unwrap();

        let mut params = default_params();
        params.stale_after_days = Some(90);
        assert_eq!(review_memories(&store, &params).await.unwrap().len(), 1);

        params.stale_after_days = Some(120);
        assert!(review_memories(&store, &params).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn recency_and_flagged_are_unioned() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let challenged = make("challenged", Status::Challenged, 0.7);
        let stale = make_aged("stale-active", 0.6, 200);
        store.create(&challenged).await.unwrap();
        store.create(&stale).await.unwrap();

        let mut params = default_params();
        params.stale_after_days = Some(90);

        let out = review_memories(&store, &params).await.unwrap();
        let ids: std::collections::HashSet<_> = out.iter().map(|m| m.id.clone()).collect();
        assert!(ids.contains(&challenged.id));
        assert!(ids.contains(&stale.id));
    }

    #[tokio::test]
    async fn recency_respects_scope_filter() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let mut in_scope = make_aged("in", 0.5, 200);
        in_scope.logical = vec!["app.core".to_string()];
        let mut out_scope = make_aged("out", 0.5, 200);
        out_scope.logical = vec!["app.ui".to_string()];
        store.create(&in_scope).await.unwrap();
        store.create(&out_scope).await.unwrap();

        let mut params = default_params();
        params.stale_after_days = Some(90);
        params.scope = Some("app.core".to_string());

        let out = review_memories(&store, &params).await.unwrap();
        let ids: Vec<_> = out.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, vec![in_scope.id]);
    }

    #[tokio::test]
    async fn count_recency_stale_counts_only_old_active() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        store.create(&make_aged("old1", 0.5, 200)).await.unwrap();
        store.create(&make_aged("old2", 0.5, 120)).await.unwrap();
        store.create(&make_aged("fresh", 0.5, 10)).await.unwrap();
        // A flagged memory is not counted by the recency helper (it counts the
        // *recency* backlog specifically, not flagged review items).
        store
            .create(&make("challenged", Status::Challenged, 0.5))
            .await
            .unwrap();

        assert_eq!(count_recency_stale(&store, Some(90)).await.unwrap(), 2);
        // Disabled trigger ⇒ zero.
        assert_eq!(count_recency_stale(&store, None).await.unwrap(), 0);
        // A 0-day window is treated as disabled (not "everything is stale").
        assert_eq!(count_recency_stale(&store, Some(0)).await.unwrap(), 0);
    }

    #[test]
    fn recency_cutoff_edge_cases() {
        let now = Utc::now();
        assert!(recency_cutoff(now, None).is_none());
        assert!(recency_cutoff(now, Some(0)).is_none());
        assert_eq!(
            recency_cutoff(now, Some(90)),
            Some(now - Duration::days(90))
        );
        // Absurdly large window overflows the duration ⇒ treated as disabled.
        assert!(recency_cutoff(now, Some(u64::MAX)).is_none());
    }

    /// §2.4 interplay: invalidated memories (closed validity windows) are
    /// resolved history, not review candidates — for BOTH arms. Without the
    /// exclusion, a Challenged memory resolved via `invalidate` reappears in
    /// review forever, and the recency trigger nominates closed history.
    #[tokio::test]
    async fn invalidated_memories_are_excluded_from_both_arms() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;

        // Flagged arm: a challenged memory whose window closed.
        let mut dead_challenged = make("invalidated challenged", Status::Challenged, 0.9);
        dead_challenged.invalidated_at = Some(Utc::now() - Duration::days(1));
        store.create(&dead_challenged).await.unwrap();

        // Recency arm: an ancient active memory whose window closed.
        let mut dead_stale = make_aged("invalidated stale", 0.8, 400);
        dead_stale.invalidated_at = Some(Utc::now() - Duration::days(1));
        store.create(&dead_stale).await.unwrap();

        // Controls that must still surface.
        store
            .create(&make("live challenged", Status::Challenged, 0.7))
            .await
            .unwrap();
        store
            .create(&make_aged("live stale", 0.6, 400))
            .await
            .unwrap();

        // Future-dated window end: still valid, still reviewable.
        let mut scheduled = make("scheduled challenged", Status::Challenged, 0.5);
        scheduled.invalidated_at = Some(Utc::now() + Duration::days(30));
        store.create(&scheduled).await.unwrap();

        let mut params = default_params();
        params.stale_after_days = Some(90);
        let listed = review_memories(&store, &params).await.unwrap();
        let summaries: Vec<&str> = listed.iter().map(|m| m.summary.as_str()).collect();
        assert!(summaries.contains(&"live challenged"), "{summaries:?}");
        assert!(summaries.contains(&"live stale"), "{summaries:?}");
        assert!(summaries.contains(&"scheduled challenged"), "{summaries:?}");
        assert!(!summaries.contains(&"invalidated challenged"), "{summaries:?}");
        assert!(!summaries.contains(&"invalidated stale"), "{summaries:?}");

        // Count helper matches the recency arm: only the live stale one.
        assert_eq!(count_recency_stale(&store, Some(90)).await.unwrap(), 1);
    }
}
