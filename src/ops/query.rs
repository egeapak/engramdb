//! Unified memory query operation.
//!
//! Thin wrapper delegating to [`RetrievalEngine::query`]. See there for the
//! semantics of [`RetrievalMode::Rank`] vs [`RetrievalMode::Filter`].

use crate::retrieval::engine::{RetrievalEngine, RetrievalQuery, RetrievalResult, ScoredMemory};
use anyhow::Result;
use std::collections::HashSet;

/// Query memories via the retrieval engine.
pub async fn query_memories(
    engine: &RetrievalEngine,
    query: &RetrievalQuery,
) -> Result<RetrievalResult> {
    let result = engine.query(query).await?;
    Ok(result)
}

/// Run `query` on `engine` and, when `include_global` is set, fold in the
/// global store's results for the same query.
///
/// This is the shared `include_global` band that both front-ends used to
/// hand-roll separately (the CLI copy even carried a comment "mirroring the
/// MCP include_global option" — the drift pattern this module exists to
/// prevent). `global_engine` is invoked lazily, only when the merge will
/// actually run; callers already querying the global store should pass
/// `include_global = false`. A `None` global engine or a failed global query
/// degrades to project-only results — the global merge is best-effort by
/// contract.
pub async fn query_memories_with_global<F, Fut>(
    engine: &RetrievalEngine,
    query: &RetrievalQuery,
    include_global: bool,
    global_engine: F,
) -> Result<RetrievalResult>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<RetrievalEngine>>,
{
    let mut result = query_memories(engine, query).await?;
    if include_global {
        if let Some(global_engine) = global_engine().await {
            if let Ok(global_result) = query_memories(&global_engine, query).await {
                let max = query.max_results.unwrap_or(10);
                let global_total = global_result.total;
                let duplicates =
                    merge_scored_memories(&mut result.memories, global_result.memories, max);
                // A memory present in both stores must not count twice —
                // subtract the duplicates the merge skipped (best-effort: we
                // can only see dupes among the returned pages).
                result.total += global_total.saturating_sub(duplicates);
            }
        }
    }
    Ok(result)
}

/// Merge `global` scored memories into `project`, deduplicating by ID,
/// re-sorting by score descending, and truncating to `max`.
///
/// Used to fold cross-project ("global") memories into a project query
/// result. Shared by the MCP `include_global` option and the CLI
/// `query --include-global` flag so both stay behaviorally identical.
///
/// Returns the number of `global` entries skipped as duplicates, so callers
/// tracking a combined `total` can avoid double-counting shared memories.
pub fn merge_scored_memories(
    project: &mut Vec<ScoredMemory>,
    global: Vec<ScoredMemory>,
    max: usize,
) -> usize {
    let existing_ids: HashSet<String> = project.iter().map(|sm| sm.memory.id.clone()).collect();
    let mut duplicates = 0;
    for sm in global {
        if existing_ids.contains(&sm.memory.id) {
            duplicates += 1;
        } else {
            project.push(sm);
        }
    }
    project.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    project.truncate(max);
    duplicates
}

/// Whether a memory from a shared (group or everyone/global) store is visible
/// to a viewer identified by `viewer_ids` — the querying project's own id plus
/// every group it subscribes to.
///
/// `audience == None` ⇒ visible to the whole store's group (the default). A
/// `Some(list)` audience restricts visibility to viewers whose id (project or
/// subscribed group) appears in the list — the per-memory sharing precision
/// half of the `audience` field. The write side lives with `create`/`update`.
pub fn audience_allows(memory: &crate::types::Memory, viewer_ids: &HashSet<String>) -> bool {
    match &memory.audience {
        None => true,
        Some(list) => list.iter().any(|id| viewer_ids.contains(id)),
    }
}

/// Run `query` on `engine` (the session's own project store) and fold in
/// results from any number of shared "extra" stores — the everyone/global
/// store and each subscribed group store — reusing [`merge_scored_memories`].
///
/// This is the N-way generalization of [`query_memories_with_global`]: the
/// global store is simply the built-in "everyone" group, one extra store among
/// the subscribed groups. `extra_engines` is invoked lazily (building an engine
/// loads the embedding model), so a caller with global disabled and no
/// subscriptions returns an empty vec and the primary result is untouched.
///
/// Two cross-repo corrections are applied to every extra store, because a
/// shared memory is authored against a *different* repo than the querying one:
/// - **Physical scope is suppressed.** Physical scopes are repo-relative, so
///   the querying repo's `path` is dropped from the extra-store query — a
///   foreign-repo path must not earn (or lose) physical-proximity score.
///   Logical scope, which is repo-independent, still applies.
/// - **Audience is enforced.** A shared memory surfaces only when
///   [`audience_allows`] passes for `viewer_ids`, giving per-memory sharing
///   precision without a second copy.
///
/// Best-effort by contract: a failed extra-store query is skipped (project
/// results still return), matching the `include_global` band it generalizes.
/// The combined `total` counts only *visible* (audience-passing) extra
/// memories, so it never leaks the existence of memories hidden from the viewer.
pub async fn query_memories_with_extra_stores<F, Fut>(
    engine: &RetrievalEngine,
    query: &RetrievalQuery,
    viewer_ids: &HashSet<String>,
    extra_engines: F,
) -> Result<RetrievalResult>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Vec<RetrievalEngine>>,
{
    let mut result = query_memories(engine, query).await?;
    let extras = extra_engines().await;
    if extras.is_empty() {
        return Ok(result);
    }
    let max = query.max_results.unwrap_or(10);
    // Cross-repo: drop the repo-relative path so physical proximity isn't
    // scored against a foreign repo's file layout.
    let mut extra_query = query.clone();
    extra_query.path = None;
    for extra in &extras {
        if let Ok(extra_result) = query_memories(extra, &extra_query).await {
            let visible: Vec<ScoredMemory> = extra_result
                .memories
                .into_iter()
                .filter(|sm| audience_allows(&sm.memory, viewer_ids))
                .collect();
            let visible_count = visible.len();
            let duplicates = merge_scored_memories(&mut result.memories, visible, max);
            result.total += visible_count.saturating_sub(duplicates);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::engine::ScoredMemory;
    use crate::scoring::ScoreBreakdown;
    use crate::types::{Memory, MemoryType, Provenance, Status, Visibility};

    fn scored(id: &str, score: f64) -> ScoredMemory {
        let now = chrono::Utc::now();
        ScoredMemory {
            memory: Memory {
                id: id.to_string(),
                type_: MemoryType::Decision,
                epistemic: MemoryType::Decision.default_epistemic(),
                valid_while: None,
                valid_from: None,
                invalidated_at: None,
                superseded_by: None,
                summary: format!("summary {id}"),
                title: None,
                content: format!("content {id}"),
                details: None,
                physical: vec![],
                logical: vec![],
                tags: vec![],
                criticality: 0.5,
                decay: None,
                provenance: Provenance::human(),
                confidence: 0.9,
                supersedes: vec![],
                status: Status::Active,
                visibility: Visibility::Shared,
                audience: None,
                challenges: vec![],
                verified_at: None,
                created_at: now,
                updated_at: now,
                accessed_at: now,
                expires_at: None,
            },
            score,
            score_breakdown: ScoreBreakdown::default(),
        }
    }

    #[test]
    fn merge_dedups_by_id_keeping_project_entry() {
        let mut project = vec![scored("shared", 0.2), scored("p1", 0.9)];
        let global = vec![scored("shared", 0.95), scored("g1", 0.4)];

        merge_scored_memories(&mut project, global, 10);

        // "shared" appears once; the project copy (score 0.2) is kept.
        let shared: Vec<_> = project.iter().filter(|m| m.memory.id == "shared").collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].score, 0.2);
        assert_eq!(project.len(), 3);
    }

    #[test]
    fn merge_sorts_desc_and_truncates() {
        let mut project = vec![scored("p1", 0.30)];
        let global = vec![scored("g1", 0.90), scored("g2", 0.10)];

        merge_scored_memories(&mut project, global, 2);

        assert_eq!(project.len(), 2);
        assert_eq!(project[0].memory.id, "g1"); // highest score first
        assert_eq!(project[1].memory.id, "p1");
    }

    #[test]
    fn merge_empty_global_is_noop_aside_from_sort_truncate() {
        let mut project = vec![scored("p1", 0.5), scored("p2", 0.9)];
        merge_scored_memories(&mut project, vec![], 10);
        assert_eq!(project.len(), 2);
        assert_eq!(project[0].memory.id, "p2");
    }

    #[test]
    fn merge_truncation_can_evict_project_entry_for_higher_global() {
        // With max=1, a higher-scoring global hit must displace the lower
        // project hit (proves truncation applies after the merge+sort).
        let mut project = vec![scored("p1", 0.10)];
        merge_scored_memories(&mut project, vec![scored("g1", 0.90)], 1);
        assert_eq!(project.len(), 1);
        assert_eq!(project[0].memory.id, "g1");
    }

    #[test]
    fn merge_with_max_zero_yields_empty() {
        let mut project = vec![scored("p1", 0.5)];
        merge_scored_memories(&mut project, vec![scored("g1", 0.9)], 0);
        assert!(project.is_empty());
    }

    fn viewer(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn audience_none_is_visible_to_any_viewer() {
        let m = scored("x", 0.5).memory; // audience defaults to None
        assert!(m.audience.is_none());
        assert!(audience_allows(&m, &viewer(&[])));
        assert!(audience_allows(&m, &viewer(&["projA"])));
    }

    #[test]
    fn audience_some_requires_project_or_group_membership() {
        let mut m = scored("x", 0.5).memory;
        m.audience = Some(vec!["projA".to_string(), "__g_grpx".to_string()]);

        // Own project id in the audience → visible.
        assert!(audience_allows(&m, &viewer(&["projA"])));
        // A subscribed group in the audience → visible (viewer_ids carries the
        // project's own id plus every group it subscribes to).
        assert!(audience_allows(&m, &viewer(&["projB", "__g_grpx"])));
        // Neither the viewer's project nor any subscribed group is listed →
        // hidden. Per-memory precision: not everyone in the store sees it.
        assert!(!audience_allows(&m, &viewer(&["projB", "__g_other"])));
        // No identity at all → hidden.
        assert!(!audience_allows(&m, &viewer(&[])));
    }
}
