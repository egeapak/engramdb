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

/// Outcome of a cross-store fan-in query.
///
/// Carries the merged [`RetrievalResult`] plus the labels of any extra stores
/// that could **not** be read. That second field is the empty-vs-corrupt
/// distinction the fan-in used to swallow: an *empty* store returns `Ok` with no
/// memories and contributes nothing (never reported); a *corrupt/unreadable*
/// store's query errors and its label lands in `unreadable` so a front-end can
/// surface it (e.g. a `doctor` hint) instead of the memory silently vanishing.
/// The query itself still succeeds regardless — fan-in is best-effort by
/// contract.
#[derive(Debug)]
pub struct ExtraStoresResult {
    /// The project result with every readable extra store merged in.
    pub result: RetrievalResult,
    /// Labels (group ids / `"global"`) of extra stores whose query failed.
    /// Empty on the happy path.
    pub unreadable: Vec<String>,
}

/// Over-fetch multiplier for the equalization candidate pool: each store is
/// queried for up to `max * FACTOR` candidates (bounded by [`EQUALIZE_POOL_CAP`])
/// so the post-merge cross-encoder can lift a genuinely-relevant memory that
/// ranked below `max` under the incomparable per-store vector scores.
const EQUALIZE_POOL_FACTOR: usize = 5;
/// Hard cap on the equalization candidate pool, keeping the cross-encoder pass
/// bounded even for a large `max`. The effective pool is never below `max`.
const EQUALIZE_POOL_CAP: usize = 100;

/// Run `query` on `engine` (the session's own project store) and fold in
/// results from any number of shared "extra" stores — the everyone/global
/// store and each subscribed group store — reusing [`merge_scored_memories`].
///
/// This is the N-way generalization of [`query_memories_with_global`]: the
/// global store is simply the built-in "everyone" group, one extra store among
/// the subscribed groups. `extra_engines` is invoked lazily (building an engine
/// loads the embedding model), so a caller with global disabled and no
/// subscriptions returns an empty vec and the primary result is untouched. Each
/// engine is paired with a **label** (its group id, or `"global"`) used only to
/// name the store in the `unreadable` report.
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
/// **Parallelized:** the extra-store queries run concurrently (`join_all`); the
/// merge then folds them in the original engine order so dedup stays
/// deterministic regardless of which store finishes first (the project entry,
/// and among extras the earlier-listed store, wins a tie).
///
/// **Cross-store rerank equalization (P2):** when `equalize_cross_store_scores`
/// is set, a single cross-encoder pass re-scores the whole merged union with one
/// model-agnostic scorer after merging — the fix for stores embedded with
/// different models producing incomparable vector scores. The caller sets it
/// only on confirmed embedding-fingerprint drift
/// ([`cross_store_equalization_needed`](crate::ops::cross_store_equalization_needed));
/// the same-fingerprint fast path leaves it `false` and this is skipped. It
/// needs a query text signal and is best-effort (a rerank failure keeps the
/// per-store scores).
///
/// Best-effort by contract: a failed extra-store query is skipped (project
/// results still return) but its label is recorded in
/// [`ExtraStoresResult::unreadable`] and logged, matching the `include_global`
/// band it generalizes. The combined `total` counts only *visible*
/// (audience-passing) extra memories, so it never leaks the existence of
/// memories hidden from the viewer.
pub async fn query_memories_with_extra_stores<F, Fut>(
    engine: &RetrievalEngine,
    query: &RetrievalQuery,
    viewer_ids: &HashSet<String>,
    equalize_cross_store_scores: bool,
    extra_engines: F,
) -> Result<ExtraStoresResult>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Vec<(String, RetrievalEngine)>>,
{
    let max = query.max_results.unwrap_or(10);
    // When equalizing, over-fetch a deeper candidate pool from *every* store so
    // the post-merge cross-encoder can lift a memory that ranked low under the
    // incomparable per-store vector scores — the whole point of equalization.
    // The final cut to `max` happens after the rerank. The pool is bounded so
    // the extra cross-encoder pass stays cheap, and is never below `max`. On the
    // common (non-equalizing) path `pool == max`, so behavior is unchanged.
    let pool = if equalize_cross_store_scores {
        max.saturating_mul(EQUALIZE_POOL_FACTOR)
            .min(EQUALIZE_POOL_CAP)
            .max(max)
    } else {
        max
    };

    // Primary (own project) query. Over-fetch to `pool` only when equalizing;
    // otherwise leave the caller's query untouched (preserving `max_results`,
    // including `None` → the engine default).
    let mut result = if equalize_cross_store_scores {
        let mut primary_query = query.clone();
        primary_query.max_results = Some(pool);
        query_memories(engine, &primary_query).await?
    } else {
        query_memories(engine, query).await?
    };

    let extras = extra_engines().await;
    if extras.is_empty() {
        // Nothing to fan in. If we over-fetched a deeper primary pool, trim it
        // back to `max` (no extras ⇒ no cross-store equalization to run).
        result.memories.truncate(max);
        return Ok(ExtraStoresResult {
            result,
            unreadable: Vec::new(),
        });
    }
    // Cross-repo: drop the repo-relative path so physical proximity isn't
    // scored against a foreign repo's file layout; over-fetch to `pool`.
    let mut extra_query = query.clone();
    extra_query.path = None;
    if equalize_cross_store_scores {
        extra_query.max_results = Some(pool);
    }
    // Fan out concurrently; each future borrows its engine and the shared
    // query immutably, so they poll together without contention.
    let extra_results =
        futures_util::future::join_all(extras.iter().map(|(_, e)| query_memories(e, &extra_query)))
            .await;
    // Fold in original order so dedup/tie-breaking is independent of completion
    // order. Merge to `pool` (== `max` unless equalizing) so the deeper pool
    // survives into the rerank.
    let mut unreadable = Vec::new();
    for ((label, _), extra_result) in extras.iter().zip(extra_results) {
        match extra_result {
            Ok(extra_result) => {
                let visible: Vec<ScoredMemory> = extra_result
                    .memories
                    .into_iter()
                    .filter(|sm| audience_allows(&sm.memory, viewer_ids))
                    .collect();
                let visible_count = visible.len();
                let duplicates = merge_scored_memories(&mut result.memories, visible, pool);
                result.total += visible_count.saturating_sub(duplicates);
            }
            Err(e) => {
                // A corrupt/unreadable extra store — distinct from an empty one,
                // which returns Ok with no memories. Skip it (best-effort), but
                // record and log the failure rather than swallowing it.
                tracing::warn!(
                    store = %label,
                    error = %e,
                    "shared store query failed; skipping it in cross-store fan-in"
                );
                unreadable.push(label.clone());
            }
        }
    }

    // P2: cross-store rerank equalization. Memories merged from stores embedded
    // with different models carry scores from different vector spaces, so their
    // relative ranking across the store boundary is arbitrary. A single
    // cross-encoder pass over the merged (over-fetched) union re-scores every
    // entry with one model-agnostic scorer and truncates to `max`, restoring a
    // comparable ranking. The caller sets `equalize_cross_store_scores` only
    // when a fingerprint actually differs (the same-fingerprint fast path leaves
    // it false), so this is off the common query path. It needs a query text
    // signal and is best-effort — a rerank failure (or no reranker configured)
    // keeps the per-store scores.
    let mut equalized = false;
    if equalize_cross_store_scores {
        if let Some(q) = query
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            match engine.rerank_merged(q, &mut result.memories, max).await {
                Ok(true) => {
                    equalized = true;
                    result.retrieval_quality = format!("{}+equalized", result.retrieval_quality);
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    error = %e,
                    "cross-store rerank equalization failed; keeping per-store scores"
                ),
            }
        }
    }
    // If we over-fetched a deeper pool but equalization did not actually run
    // (no reranker, no query text, weight 0, or a rerank error), trim back to
    // the caller's `max`. When it did run, `rerank_merged` already truncated.
    if pool > max && !equalized {
        result.memories.truncate(max);
    }

    Ok(ExtraStoresResult { result, unreadable })
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

    // End-to-end fan-in: a memory created in a real group store surfaces in a
    // subscribed project's query (filter mode, keyword-only — no embeddings),
    // and a per-memory `audience` restriction hides it from a non-listed
    // viewer while still showing it to a listed one. This exercises the N-way
    // fan-in the CLI/MCP query handlers drive, without any ML dependency.
    #[tokio::test]
    async fn extra_stores_fan_in_group_respects_audience() {
        use crate::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalQuery};
        use crate::storage::{InMemoryRegistry, MemoryStore};
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
        use tempfile::TempDir;

        // Querying project store (the viewer) — starts empty.
        let proj_dir = TempDir::new().unwrap();
        let proj_store = MemoryStore::init(proj_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let proj_id = proj_store.project_id.clone();

        // A real group store shared across projects.
        let gid = crate::storage::paths::compute_group_id("fan-in-audience-test");
        let group_store = MemoryStore::init_group(&gid).await.unwrap();

        // Group-wide memory (audience None) — visible to any subscriber.
        let open = Memory::new(
            MemoryType::Convention,
            "open widget convention",
            "the widget content is shared",
            Provenance::human(),
        );
        group_store.create(&open).await.unwrap();

        // Restricted memory (audience = a different project) — per-memory
        // precision: only a listed viewer may see it.
        let mut restricted = Memory::new(
            MemoryType::Convention,
            "restricted widget rule",
            "the widget content is restricted",
            Provenance::human(),
        );
        restricted.audience = Some(vec!["restricted-target-proj".to_string()]);
        group_store.create(&restricted).await.unwrap();
        drop(group_store);

        let mut config = EngramConfig::default();
        config.retrieval.relevance_threshold = 0.0; // keep all keyword candidates
        let proj_engine = RetrievalEngine::new(proj_store, config.clone());

        let query = RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("widget".to_string()),
            max_results: Some(10),
            ..Default::default()
        };

        // Viewer 1: only the project's own id (NOT the restricted audience).
        let viewer_plain: HashSet<String> = [proj_id.clone()].into_iter().collect();
        let cfg = config.clone();
        let gid1 = gid.clone();
        let result = query_memories_with_extra_stores(
            &proj_engine,
            &query,
            &viewer_plain,
            false,
            || async move {
                vec![(
                    gid1.clone(),
                    RetrievalEngine::new(MemoryStore::open_group(&gid1).await.unwrap(), cfg),
                )]
            },
        )
        .await
        .unwrap()
        .result;
        assert!(
            result
                .memories
                .iter()
                .any(|m| m.memory.summary.contains("open widget")),
            "group-wide (audience None) memory must surface for a subscriber"
        );
        assert!(
            !result
                .memories
                .iter()
                .any(|m| m.memory.summary.contains("restricted")),
            "audience-restricted memory must be hidden from a non-listed viewer"
        );

        // Viewer 2: carries the restricted audience id → now sees it.
        let viewer_listed: HashSet<String> = [proj_id, "restricted-target-proj".to_string()]
            .into_iter()
            .collect();
        let cfg2 = config.clone();
        let gid2 = gid.clone();
        let result2 = query_memories_with_extra_stores(
            &proj_engine,
            &query,
            &viewer_listed,
            false,
            || async move {
                vec![(
                    gid2.clone(),
                    RetrievalEngine::new(MemoryStore::open_group(&gid2).await.unwrap(), cfg2),
                )]
            },
        )
        .await
        .unwrap()
        .result;
        assert!(
            result2
                .memories
                .iter()
                .any(|m| m.memory.summary.contains("restricted")),
            "audience-restricted memory must surface for a listed viewer"
        );
    }

    // A corrupt/unreadable extra store must be *reported* (its label in
    // `unreadable`) rather than silently swallowed — and must NOT fail the whole
    // query: the project result and any healthy extra stores still come back.
    // This is the empty-vs-corrupt distinction on the query path (an empty store
    // returns Ok with no memories and is never reported).
    #[tokio::test]
    async fn extra_stores_corrupt_store_is_reported_not_fatal() {
        use crate::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalQuery};
        use crate::storage::{InMemoryRegistry, MemoryStore};
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
        use tempfile::TempDir;

        let proj_dir = TempDir::new().unwrap();
        let proj_store = MemoryStore::init(proj_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        // A healthy group with one group-wide memory.
        let gid_ok = crate::storage::paths::compute_group_id("corrupt-test-healthy");
        let group_ok = MemoryStore::init_group(&gid_ok).await.unwrap();
        group_ok
            .create(&Memory::new(
                MemoryType::Convention,
                "healthy widget convention",
                "the widget content is healthy",
                Provenance::human(),
            ))
            .await
            .unwrap();
        drop(group_ok);

        // A "corrupt" group: initialize it, build its engine (open succeeds),
        // then delete its LanceDB directory out from under the open handle so
        // the subsequent query errors.
        let gid_bad = crate::storage::paths::compute_group_id("corrupt-test-broken");
        let _group_bad = MemoryStore::init_group(&gid_bad).await.unwrap();
        drop(_group_bad);

        let mut config = EngramConfig::default();
        config.retrieval.relevance_threshold = 0.0;
        let proj_engine = RetrievalEngine::new(proj_store, config.clone());

        let query = RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("widget".to_string()),
            max_results: Some(10),
            ..Default::default()
        };

        let viewer: HashSet<String> = HashSet::new();
        let cfg_ok = config.clone();
        let cfg_bad = config.clone();
        let gid_ok_c = gid_ok.clone();
        let gid_bad_c = gid_bad.clone();
        let outcome =
            query_memories_with_extra_stores(&proj_engine, &query, &viewer, false, || async move {
                let ok_engine =
                    RetrievalEngine::new(MemoryStore::open_group(&gid_ok_c).await.unwrap(), cfg_ok);
                let bad_engine = RetrievalEngine::new(
                    MemoryStore::open_group(&gid_bad_c).await.unwrap(),
                    cfg_bad,
                );
                // Corrupt the bad group after its engine is built.
                let bad_lance = crate::storage::paths::group_lancedb_dir(&gid_bad_c).unwrap();
                let _ = std::fs::remove_dir_all(&bad_lance);
                std::fs::write(&bad_lance, b"not a directory").unwrap();
                vec![
                    (gid_ok_c.clone(), ok_engine),
                    (gid_bad_c.clone(), bad_engine),
                ]
            })
            .await
            .unwrap();

        // The healthy group's memory still surfaces...
        assert!(
            outcome
                .result
                .memories
                .iter()
                .any(|m| m.memory.summary.contains("healthy widget")),
            "a healthy extra store must still merge in despite a sibling being corrupt"
        );
        // ...and the corrupt group is reported, not swallowed.
        assert!(
            outcome.unreadable.contains(&gid_bad),
            "a corrupt extra store must be reported in `unreadable`, got {:?}",
            outcome.unreadable
        );
        assert!(
            !outcome.unreadable.contains(&gid_ok),
            "the healthy store must not be reported unreadable"
        );
    }

    // Regression for the P2 review: equalization must be able to lift a memory
    // that ranked BELOW `max` under the incomparable per-store scores. That only
    // works if the fan-in over-fetches a deeper pool when equalizing (otherwise
    // the good memory is truncated away before the cross-encoder ever sees it).
    // A deterministic marker reranker on the project engine scores the marked
    // memory highest; without the over-fetch the marked memory (lowest prior
    // score, rank > max) would never reach the rerank.
    #[tokio::test]
    async fn equalization_lifts_a_sub_max_memory_via_overfetch() {
        use crate::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalQuery};
        use crate::retrieval::reranker::{RerankScore, Reranker};
        use crate::storage::{InMemoryRegistry, MemoryStore};
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
        use tempfile::TempDir;

        struct MarkerReranker;
        #[async_trait::async_trait]
        impl Reranker for MarkerReranker {
            async fn rerank(&self, _q: &str, docs: &[String]) -> anyhow::Result<Vec<RerankScore>> {
                Ok(docs
                    .iter()
                    .enumerate()
                    .map(|(index, d)| RerankScore {
                        index,
                        score: if d.contains("LIFTME") { 12.0 } else { -12.0 },
                    })
                    .collect())
            }
        }

        let proj_dir = TempDir::new().unwrap();
        let proj_store = MemoryStore::init(proj_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        // Group store: six high-criticality fillers rank above `max`, and one
        // low-criticality marked memory ranks below it.
        let gid = crate::storage::paths::compute_group_id("overfetch-equalize-test");
        let group = MemoryStore::init_group(&gid).await.unwrap();
        for i in 0..6 {
            let mut m = Memory::new(
                MemoryType::Convention,
                format!("widget filler {i}"),
                "widget content",
                Provenance::human(),
            );
            m.criticality = 0.9;
            group.create(&m).await.unwrap();
        }
        let mut winner = Memory::new(
            MemoryType::Convention,
            "widget LIFTME winner",
            "widget content",
            Provenance::human(),
        );
        winner.criticality = 0.05;
        group.create(&winner).await.unwrap();
        drop(group);

        let mut config = EngramConfig::default();
        config.retrieval.relevance_threshold = 0.0;
        config.rerank.enabled = true;
        config.rerank.weight = 1.0;

        let proj_engine = RetrievalEngine::new(proj_store, config.clone())
            .with_reranker(std::sync::Arc::new(MarkerReranker));

        let query = RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("widget".to_string()),
            max_results: Some(3), // marked memory ranks ~7th → excluded without over-fetch
            ..Default::default()
        };
        let viewer: HashSet<String> = HashSet::new();
        let cfg = config.clone();
        let gid_c = gid.clone();
        // equalize = true: the fan-in over-fetches, so the marked memory is in
        // the merged pool for the cross-encoder to lift.
        let outcome =
            query_memories_with_extra_stores(&proj_engine, &query, &viewer, true, || async move {
                vec![(
                    gid_c.clone(),
                    RetrievalEngine::new(MemoryStore::open_group(&gid_c).await.unwrap(), cfg),
                )]
            })
            .await
            .unwrap();

        assert!(
            outcome
                .result
                .memories
                .first()
                .is_some_and(|m| m.memory.summary.contains("LIFTME")),
            "over-fetch + equalization must surface and lift the sub-`max` marked memory; got: {:?}",
            outcome
                .result
                .memories
                .iter()
                .map(|m| m.memory.summary.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            outcome.result.retrieval_quality.contains("+equalized"),
            "retrieval_quality must record that equalization ran"
        );
        assert!(
            outcome.result.memories.len() <= 3,
            "final result must be truncated to max"
        );
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
