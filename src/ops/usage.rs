//! Per-memory usage recording and reads — the policy layer over
//! `engram_storage::telemetry::{usage, memory_events}`.
//!
//! **Where recording happens.** Every write op in this module tree (`create`,
//! `update`, `delete`, `challenge`, `resolve`, `verify`, `compress`,
//! consolidation, promotion) takes a [`Surface`] and records its own event, so
//! the CLI and the MCP server — which both pass through here — never repeat
//! it; `gc`'s deletions, consolidation and auto-promotion record under
//! [`Surface::Maintenance`]. The three read signals are recorded where only a
//! front end knows what the caller saw: `returned` after a CLI or MCP `query`
//! and the MCP `memory://context` resource and `memory-session-start` prompt
//! (never inside the engine, which hooks and maintenance also drive),
//! `fetched` after a CLI or
//! MCP `get` (never in `MemoryStore::get`, which `update` uses internally),
//! and `injected` by the hooks, for exactly the memories that fit the
//! budget. `reindex` and `migrate` record nothing: they change no meaning.
//!
//! **Gate.** `[stats].enabled` of the store being written, or of the querying
//! project for `returned` (whose memories may belong to a group or the global
//! store: those events still land in the *owner's* log). Recording is
//! best-effort: a failure is logged at debug level and never fails the op.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalResult};
use crate::storage::MemoryStore;
use crate::types::EngramConfig;

pub use crate::telemetry::memory_events::MemoryUsage;
pub use crate::telemetry::usage::{
    load_current_usage, load_daily_series, load_rollup, load_usage, returned_sessions,
    top_memories, DailyUsage, MemoryEvent, Surface, UsageCounts, UsageKind, UsageSort, UsageView,
    DEFAULT_SERIES_DAYS,
};

/// A store a memory may belong to: its id (the event's `store_id`) and the
/// root whose `.engramdb/state/` holds its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRef {
    pub id: String,
    pub root: PathBuf,
}

impl StoreRef {
    pub fn of(store: &MemoryStore) -> Self {
        Self {
            id: store.project_id.clone(),
            root: store.project_dir.clone(),
        }
    }
}

async fn store_config(store: &MemoryStore) -> EngramConfig {
    config_at(&store.project_dir).await
}

async fn config_at(root: &std::path::Path) -> EngramConfig {
    crate::storage::config::load_config_or_default(&root.join(".engramdb").join("config.toml"))
        .await
}

/// The session id a CLI invocation runs under, when it knows one: the
/// variables `task current` and the MCP server already honor. A query run
/// from a plain terminal has none, and its `returned` events then never
/// count toward promotion (which counts distinct sessions).
pub fn cli_session_id() -> Option<String> {
    ["CLAUDE_SESSION_ID", "MCP_SESSION_ID"]
        .into_iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
}

/// Whether `store` records usage (`[stats].enabled`).
pub async fn usage_enabled(store: &MemoryStore) -> bool {
    store_config(store).await.stats.enabled
}

/// Append `events` to `owner`'s log. The caller has checked the gate.
pub fn write_events(owner: &StoreRef, events: &[MemoryEvent]) {
    if let Err(e) = crate::telemetry::usage::record_memory_events(&owner.root, events) {
        tracing::debug!(store = %owner.id, "usage: could not record events (non-fatal): {e}");
    }
}

/// Record one write event for `id` (a full id or a unique prefix) in
/// `store`'s log, when the store records usage.
pub(crate) async fn record_write(
    store: &MemoryStore,
    id: &str,
    kind: UsageKind,
    surface: Surface,
    related_id: Option<&str>,
) {
    if !usage_enabled(store).await {
        return;
    }
    // Ops accept prefixes; events always carry the full id. Resolved only
    // here, behind the gate, so a disabled store pays nothing, and only for
    // a prefix: a full id needs no index query.
    let full = if uuid::Uuid::parse_str(id).is_ok() {
        id.to_string()
    } else {
        store
            .resolve_id(id)
            .await
            .unwrap_or_else(|_| id.to_string())
    };
    record_resolved(store, &full, kind, surface, related_id);
}

/// [`record_write`] for an id that is already full and a gate already
/// checked, e.g. after a delete, when the id no longer resolves.
pub(crate) fn record_resolved(
    store: &MemoryStore,
    full_id: &str,
    kind: UsageKind,
    surface: Surface,
    related_id: Option<&str>,
) {
    let event =
        MemoryEvent::new(full_id, &store.project_id, kind, surface).with_related(related_id);
    write_events(&StoreRef::of(store), &[event]);
}

/// Record `fetched` for a memory read by id at a front end (`get`).
pub async fn record_fetched(
    store: &MemoryStore,
    memory_id: &str,
    surface: Surface,
    session_id: Option<&str>,
) {
    if !usage_enabled(store).await {
        return;
    }
    let event = MemoryEvent::new(memory_id, &store.project_id, UsageKind::Fetched, surface)
        .with_session(session_id);
    write_events(&StoreRef::of(store), &[event]);
}

/// Record `returned` for every memory in a query answer, in its owner's log.
///
/// `owners` maps the ids that came from a fanned-in group or global store
/// ([`crate::ops::ExtraStoresResult::owners`]); every other id belongs to
/// `engine`'s own store. Gated on `engine`'s config — the querying project's
/// choice. One write per owning store.
pub fn record_returned(
    engine: &RetrievalEngine,
    result: &RetrievalResult,
    owners: &HashMap<String, StoreRef>,
    mode: RetrievalMode,
    surface: Surface,
    session_id: Option<&str>,
) {
    if !engine.config().stats.enabled || result.memories.is_empty() {
        return;
    }
    let primary = StoreRef::of(engine.store());
    let mode = match mode {
        RetrievalMode::Rank => "rank",
        RetrievalMode::Filter => "filter",
    };
    let mut by_owner: Vec<(StoreRef, Vec<MemoryEvent>)> = Vec::new();
    for (i, sm) in result.memories.iter().enumerate() {
        let owner = owners.get(&sm.memory.id).unwrap_or(&primary);
        let event = MemoryEvent::new(&sm.memory.id, &owner.id, UsageKind::Returned, surface)
            .with_session(session_id)
            .with_rank(i + 1, sm.score)
            .with_query_mode(mode);
        match by_owner.iter_mut().find(|(o, _)| o == owner) {
            Some((_, events)) => events.push(event),
            None => by_owner.push((owner.clone(), vec![event])),
        }
    }
    for (owner, events) in &by_owner {
        write_events(owner, events);
    }
}

/// Drain one store's usage log into its tables, prune raw events past
/// `retention_days` (folding them into the rollup first), reconcile rollup
/// rows a failed refresh left behind, and compact. Best-effort: failures
/// are logged.
pub async fn maintain_store_usage(store: &StoreRef, enabled: bool, retention_days: Option<u64>) {
    use crate::telemetry::usage as u;
    if enabled {
        if let Err(e) = u::drain_to_table(&store.root, &store.id).await {
            tracing::debug!(store = %store.id, "usage stats: drain failed (non-fatal): {e}");
        }
    }
    if let Some(days) = retention_days {
        if let Err(e) = u::prune_memory_events(&store.id, days).await {
            tracing::debug!(store = %store.id, "usage stats: prune failed (non-fatal): {e}");
        }
    }
    if let Err(e) = u::reconcile_rollup(&store.id).await {
        tracing::debug!(store = %store.id, "usage stats: reconcile failed (non-fatal): {e}");
    }
    if let Err(e) = u::optimize_memory_events(&store.id).await {
        tracing::debug!(store = %store.id, "usage stats: optimize failed (non-fatal): {e}");
    }
}

/// Usage housekeeping for a project: its own log, plus the logs of every
/// shared store its queries fan into (subscribed groups and the global
/// store). A `returned` event for a group memory is written to the group's
/// log, and nothing else drains a group store, so the subscribers do it.
/// Each shared store is maintained under its *own* `[stats]` settings: two
/// subscribers with different retention would otherwise prune the same
/// store to whichever ran last. `gc` and the throttled CLI maintenance pass
/// call this; the daemon calls [`maintain_project_usage`] as it serves hooks.
pub async fn maintain_memory_events(store: &MemoryStore) {
    let config = store_config(store).await;
    maintain_project_usage(
        &StoreRef::of(store),
        store.is_group() || store.is_global(),
        config.stats.enabled,
        config.stats.retention_days,
    )
    .await;
}

/// [`maintain_memory_events`] for a store known by id and root. `shared`
/// marks a group or the global store, which has no subscriptions to follow.
pub async fn maintain_project_usage(
    store: &StoreRef,
    shared: bool,
    enabled: bool,
    retention_days: Option<u64>,
) {
    maintain_store_usage(store, enabled, retention_days).await;
    if shared {
        return;
    }
    for other in shared_stores_of(&store.id).await {
        let own = config_at(&other.root).await;
        maintain_store_usage(&other, own.stats.enabled, own.stats.retention_days).await;
    }
}

/// The group and global stores whose logs a project's queries may write:
/// its subscriptions and the global store, each only when its directory
/// exists (never created here).
async fn shared_stores_of(project_id: &str) -> Vec<StoreRef> {
    let mut out = Vec::new();
    if let Ok(registry) = crate::storage::FileRegistry::global() {
        use crate::storage::RegistryBackend;
        if let Ok(reg) = registry.load().await {
            for gid in crate::storage::registry::subscriptions_of(&reg, project_id) {
                if let Ok(root) = crate::storage::paths::group_store_dir(gid) {
                    if root.is_dir() {
                        out.push(StoreRef {
                            id: gid.clone(),
                            root,
                        });
                    }
                }
            }
        }
    }
    if let Ok(root) = crate::storage::paths::global_store_dir() {
        if root.is_dir() {
            out.push(StoreRef {
                id: crate::storage::paths::GLOBAL_PROJECT_ID.to_string(),
                root,
            });
        }
    }
    out
}

/// Usage of one memory: exact lifetime counts plus its daily series.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryUsageReport {
    pub memory_id: String,
    pub summary: String,
    pub store_id: String,
    #[serde(flatten)]
    pub counts: UsageCounts,
    /// The last [`DEFAULT_SERIES_DAYS`] days, oldest first.
    pub daily: Vec<DailyUsage>,
}

/// Usage of the memory `id` (a full id or unique prefix) in `store`, or, if
/// `store` has no such memory, in the first shared store (subscribed group,
/// then global) that does: a group memory's events are in the group's log.
///
/// The fallback opens those shared stores (an open may migrate one). A
/// read-only consumer that already holds its stores calls
/// [`memory_usage_report_in`] instead, which opens and creates nothing.
pub async fn memory_usage_report(
    store: &MemoryStore,
    id: &str,
) -> anyhow::Result<MemoryUsageReport> {
    match memory_usage_report_in(&[store], id).await {
        Err(e)
            if matches!(
                e.downcast_ref::<crate::storage::StorageError>(),
                Some(crate::storage::StorageError::NotFound(_))
            ) && !(store.is_group() || store.is_global()) =>
        {
            let mut shared = Vec::new();
            for s in shared_stores_of(&store.project_id).await {
                let opened = if s.id == crate::storage::paths::GLOBAL_PROJECT_ID {
                    MemoryStore::open_global().await
                } else {
                    MemoryStore::open_group(&s.id).await
                };
                if let Ok(opened) = opened {
                    shared.push(opened);
                }
            }
            let refs: Vec<&MemoryStore> = shared.iter().collect();
            if refs.is_empty() {
                return Err(e);
            }
            memory_usage_report_in(&refs, id).await
        }
        other => other,
    }
}

/// [`memory_usage_report`] over stores the caller already opened, tried in
/// order: the first that holds `id` owns it. Opens, creates and migrates
/// nothing. `StorageError::NotFound` when none holds it; an ambiguous
/// prefix is the store's own `Validation` error, and a failed usage read is
/// an error rather than a zero.
pub async fn memory_usage_report_in(
    stores: &[&MemoryStore],
    id: &str,
) -> anyhow::Result<MemoryUsageReport> {
    let mut found = None;
    for store in stores {
        match store.get(id).await {
            Ok(m) => {
                found = Some((StoreRef::of(store), m));
                break;
            }
            Err(crate::storage::StorageError::NotFound(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let (owner, memory) =
        found.ok_or_else(|| crate::storage::StorageError::NotFound(id.to_string()))?;
    let view = load_usage(&owner.root, &owner.id, Some(&memory.id)).await?;
    let counts = view
        .by_memory
        .get(&memory.id)
        .map(|u| u.counts.clone())
        .unwrap_or_default();
    let daily = load_daily_series(
        &owner.root,
        &owner.id,
        Some(&memory.id),
        DEFAULT_SERIES_DAYS,
    )
    .await?;
    Ok(MemoryUsageReport {
        memory_id: memory.id,
        summary: memory.summary,
        store_id: owner.id,
        counts,
        daily,
    })
}

/// One row of a top-N listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TopMemory {
    pub memory_id: String,
    /// `None` when the memory no longer exists (deleted since it was used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(flatten)]
    pub counts: UsageCounts,
}

/// The store's `n` most-used memories by `sort`, with their summaries. Reads
/// [`load_current_usage`] (the rollup plus the undrained log): one row per
/// memory, no raw scan, nothing created. Distinct-session counts there are
/// an upper bound; the per-memory report is exact.
pub async fn top_used(
    store: &MemoryStore,
    n: usize,
    sort: UsageSort,
) -> anyhow::Result<Vec<TopMemory>> {
    let view = load_current_usage(&store.project_dir, &store.project_id).await?;
    let top = top_memories(&view, n, sort);
    let ids: Vec<&str> = top.iter().map(|u| u.memory_id.as_str()).collect();
    let summaries: HashMap<String, String> = match store.get_batch(&ids).await {
        Ok(found) => found.into_iter().map(|(id, m)| (id, m.summary)).collect(),
        Err(_) => HashMap::new(),
    };
    Ok(top
        .into_iter()
        .map(|u| TopMemory {
            summary: summaries.get(&u.memory_id).cloned(),
            memory_id: u.memory_id,
            counts: u.counts,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    pub(crate) async fn store() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    pub(crate) fn logged(store: &MemoryStore) -> Vec<MemoryEvent> {
        crate::telemetry::usage::load_memory_events(&store.project_dir)
            .unwrap()
            .events
    }

    #[tokio::test]
    async fn record_write_resolves_prefixes_and_names_the_owner() {
        let (_t, store) = store().await;
        let m = Memory::new(MemoryType::Decision, "s", "c", Provenance::human());
        let id = store.create(&m).await.unwrap();
        record_write(&store, &id[..8], UsageKind::Verified, Surface::Mcp, None).await;
        let events = logged(&store);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].memory_id, id);
        assert_eq!(events[0].store_id, store.project_id);
        assert_eq!(
            (events[0].kind, events[0].surface),
            (UsageKind::Verified, Surface::Mcp)
        );
    }

    #[tokio::test]
    async fn stats_disabled_records_nothing() {
        let (tmp, store) = store().await;
        std::fs::write(
            tmp.path().join(".engramdb/config.toml"),
            "[stats]\nenabled = false\n",
        )
        .unwrap();
        record_write(&store, "x", UsageKind::Updated, Surface::Cli, None).await;
        record_fetched(&store, "x", Surface::Cli, None).await;
        assert!(logged(&store).is_empty());
    }

    #[tokio::test]
    async fn returned_lands_in_each_owners_log_with_rank_and_mode() {
        let (_t, store) = store().await;
        let (_g, other) = self::store().await;
        let a = store
            .create(&Memory::new(
                MemoryType::Decision,
                "a",
                "c",
                Provenance::human(),
            ))
            .await
            .unwrap();
        let engine = RetrievalEngine::new(store.clone(), EngramConfig::default());
        let scored = |id: &str, score| crate::retrieval::engine::ScoredMemory {
            memory: Memory {
                id: id.to_string(),
                ..Memory::new(MemoryType::Decision, "s", "c", Provenance::human())
            },
            score,
            score_breakdown: Default::default(),
        };
        let result = RetrievalResult {
            memories: vec![scored(&a, 0.9), scored("foreign", 0.5)],
            total: 2,
            retrieval_quality: "keyword_only".to_string(),
            dropped_below_threshold: 0,
        };
        let mut owners = HashMap::new();
        owners.insert("foreign".to_string(), StoreRef::of(&other));
        record_returned(
            &engine,
            &result,
            &owners,
            RetrievalMode::Filter,
            Surface::Cli,
            Some("sess"),
        );
        let own = logged(&store);
        assert_eq!(own.len(), 1);
        assert_eq!(
            (own[0].rank, own[0].query_mode.as_deref()),
            (Some(1), Some("filter"))
        );
        assert_eq!(own[0].session_id.as_deref(), Some("sess"));
        let foreign = logged(&other);
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].store_id, other.project_id);
        assert_eq!(foreign[0].rank, Some(2));
    }

    #[tokio::test]
    async fn report_and_top_used_read_the_log() {
        let (_t, store) = store().await;
        let id = store
            .create(&Memory::new(
                MemoryType::Decision,
                "the summary",
                "c",
                Provenance::human(),
            ))
            .await
            .unwrap();
        record_fetched(&store, &id, Surface::Cli, None).await;
        record_fetched(&store, &id, Surface::Mcp, Some("s")).await;
        let report = memory_usage_report(&store, &id[..8]).await.unwrap();
        assert_eq!(report.memory_id, id);
        assert_eq!(report.summary, "the summary");
        assert_eq!(report.counts.fetched, 2);
        assert_eq!(report.daily.len(), DEFAULT_SERIES_DAYS as usize);
        assert_eq!(report.daily.last().unwrap().fetched, 2);

        let top = top_used(&store, 5, UsageSort::Fetched).await.unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].summary.as_deref(), Some("the summary"));
        assert!(memory_usage_report(&store, "nope").await.is_err());
    }

    fn create_params(summary: &str) -> crate::ops::CreateParams {
        crate::ops::CreateParams {
            type_: MemoryType::Decision,
            content: "content".to_string(),
            summary: summary.to_string(),
            title: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: crate::types::Visibility::Shared,
            provenance: Provenance::human(),
            supersedes: vec![],
            audience: None,
            epistemic: None,
            premise: None,
            invalidated_by: vec![],
            origin_task: None,
            generality: None,
            valid_from: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title_strategy: crate::title::TitleStrategy::None,
            embed_async: false,
        }
    }

    fn update_params() -> crate::ops::UpdateParams {
        crate::ops::UpdateParams {
            type_: None,
            content: Some("changed".to_string()),
            summary: None,
            title: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            details: None,
            visibility: None,
            status: None,
            supersedes: None,
            audience: None,
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: false,
            clear_invalidated: false,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            embed_async: false,
        }
    }

    fn kinds(store: &MemoryStore) -> Vec<(String, UsageKind, Surface, Option<String>)> {
        logged(store)
            .into_iter()
            .map(|e| (e.memory_id, e.kind, e.surface, e.related_id))
            .collect()
    }

    /// Every write op records its own kind, under the surface its caller
    /// passed, with the full id — whatever prefix the caller used.
    #[tokio::test]
    async fn every_write_op_records_its_kind_and_surface() {
        use crate::ops::{ResolveAction, ResolveParams};
        let (_t, store) = store().await;
        let a = crate::ops::create_memory(&store, create_params("a"), None, Surface::Mcp)
            .await
            .unwrap()
            .id;
        let b = crate::ops::create_memory(&store, create_params("b"), None, Surface::Cli)
            .await
            .unwrap()
            .id;
        crate::ops::update_memory(&store, &a[..24], update_params(), None, Surface::Mcp)
            .await
            .unwrap();
        crate::ops::challenge_memory(&store, &a, "evidence", None, Surface::Cli)
            .await
            .unwrap();
        crate::ops::resolve_memory(
            &store,
            ResolveParams {
                id: a.clone(),
                action: ResolveAction::Keep,
                updated_content: None,
                updated_summary: None,
                superseded_by: None,
            },
            Surface::Mcp,
        )
        .await
        .unwrap();
        crate::ops::verify_memory(&store, &a, Surface::Cli)
            .await
            .unwrap();
        crate::ops::resolve_memory(
            &store,
            ResolveParams {
                id: b.clone(),
                action: ResolveAction::Invalidate,
                updated_content: None,
                updated_summary: None,
                superseded_by: Some(a.clone()),
            },
            Surface::Cli,
        )
        .await
        .unwrap();
        let none = None::<String>;
        assert_eq!(
            kinds(&store),
            vec![
                (a.clone(), UsageKind::Created, Surface::Mcp, none.clone()),
                (b.clone(), UsageKind::Created, Surface::Cli, none.clone()),
                (a.clone(), UsageKind::Updated, Surface::Mcp, none.clone()),
                (a.clone(), UsageKind::Challenged, Surface::Cli, none.clone()),
                (a.clone(), UsageKind::Resolved, Surface::Mcp, none.clone()),
                (a.clone(), UsageKind::Verified, Surface::Cli, none.clone()),
                (
                    b.clone(),
                    UsageKind::Resolved,
                    Surface::Cli,
                    Some(a.clone())
                ),
            ]
        );

        let c = crate::ops::create_memory(&store, create_params("c"), None, Surface::Cli)
            .await
            .unwrap()
            .id;
        let d = crate::ops::create_memory(&store, create_params("d"), None, Surface::Cli)
            .await
            .unwrap()
            .id;
        let merged = crate::ops::compress_apply(
            &store,
            crate::ops::CompressApplyParams {
                source_ids: vec![c.clone(), d.clone()],
                summary: "merged".to_string(),
                content: "both".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
            Surface::Mcp,
        )
        .await
        .unwrap()
        .new_id;
        crate::ops::delete_memory(&store, &merged[..24], Surface::Mcp)
            .await
            .unwrap();
        let tail: Vec<_> = kinds(&store).into_iter().skip(9).collect();
        assert_eq!(
            tail,
            vec![
                (
                    merged.clone(),
                    UsageKind::Created,
                    Surface::Mcp,
                    none.clone()
                ),
                (
                    c.clone(),
                    UsageKind::Merged,
                    Surface::Mcp,
                    Some(merged.clone())
                ),
                (
                    d.clone(),
                    UsageKind::Merged,
                    Surface::Mcp,
                    Some(merged.clone())
                ),
                (
                    merged.clone(),
                    UsageKind::Deleted,
                    Surface::Mcp,
                    none.clone()
                ),
            ]
        );
        assert!(logged(&store)
            .iter()
            .all(|e| e.store_id == store.project_id));
    }

    #[tokio::test]
    async fn resolve_delete_records_resolved_then_deleted() {
        let (_t, store) = store().await;
        let id = crate::ops::create_memory(&store, create_params("x"), None, Surface::Cli)
            .await
            .unwrap()
            .id;
        crate::ops::resolve_memory(
            &store,
            crate::ops::ResolveParams {
                id: id[..8].to_string(),
                action: crate::ops::ResolveAction::Delete,
                updated_content: None,
                updated_summary: None,
                superseded_by: None,
            },
            Surface::Cli,
        )
        .await
        .unwrap();
        let k: Vec<_> = kinds(&store)
            .into_iter()
            .map(|(m, k, _, _)| (m, k))
            .collect();
        assert_eq!(
            k,
            vec![
                (id.clone(), UsageKind::Created),
                (id.clone(), UsageKind::Resolved),
                (id, UsageKind::Deleted),
            ]
        );
    }

    #[tokio::test]
    async fn maintenance_drains_into_the_rollup() {
        let (_t, store) = store().await;
        record_fetched(&store, "m1", Surface::Cli, None).await;
        maintain_memory_events(&store).await;
        assert!(logged(&store).is_empty(), "drained");
        let rollup = load_rollup(&store.project_id).await.unwrap();
        assert_eq!(rollup["m1"].counts.fetched, 1);
    }

    /// A group memory's events end-to-end: the fan-in names the group as
    /// owner, `returned` lands in the group's log (not the project's), the
    /// subscriber's maintenance drains it into the group's own `lancedb/`,
    /// and the report finds the memory through the subscription.
    #[tokio::test]
    async fn group_memory_usage_lands_in_the_group_and_a_subscriber_drains_it() {
        use crate::retrieval::engine::RetrievalQuery;
        use crate::storage::{FileRegistry, RegistryBackend};
        let tmp = TempDir::new().unwrap();
        let registry = FileRegistry::global().unwrap();
        let project = MemoryStore::init(tmp.path(), &registry).await.unwrap();
        let gid = crate::storage::paths::compute_group_id("usage-e2e");
        let group = MemoryStore::init_group(&gid).await.unwrap();
        registry.subscribe(&project.project_id, &gid).await.unwrap();
        let gm = group
            .create(&Memory::new(
                MemoryType::Convention,
                "group widget convention",
                "the widget content is shared",
                Provenance::human(),
            ))
            .await
            .unwrap();

        let mut config = EngramConfig::default();
        config.retrieval.relevance_threshold = 0.0;
        let engine = RetrievalEngine::new(project.clone(), config.clone());
        let query = RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("widget".to_string()),
            max_results: Some(10),
            ..Default::default()
        };
        let viewers: std::collections::HashSet<String> =
            [project.project_id.clone()].into_iter().collect();
        let g = gid.clone();
        let out = crate::ops::query_memories_with_extra_stores(
            &engine,
            &query,
            &viewers,
            false,
            || async move {
                vec![(
                    g.clone(),
                    RetrievalEngine::new(MemoryStore::open_group(&g).await.unwrap(), config),
                )]
            },
        )
        .await
        .unwrap();
        assert_eq!(
            out.owners.get(&gm).map(|o| o.id.as_str()),
            Some(gid.as_str())
        );
        record_returned(
            &engine,
            &out.result,
            &out.owners,
            RetrievalMode::Filter,
            Surface::Mcp,
            Some("s1"),
        );
        assert!(logged(&project).is_empty(), "not the asker's log");
        assert_eq!(logged(&group).len(), 1);

        maintain_memory_events(&project).await;
        assert!(logged(&group).is_empty(), "the subscriber drained it");
        assert_eq!(load_rollup(&gid).await.unwrap()[&gm].counts.returned, 1);
        assert!(crate::storage::paths::group_lancedb_dir(&gid)
            .unwrap()
            .join("memory_events.lance")
            .exists());

        let report = memory_usage_report(&project, &gm).await.unwrap();
        assert_eq!(
            (report.store_id.as_str(), report.counts.returned),
            (gid.as_str(), 1)
        );
        let missing = memory_usage_report_in(&[&project], &gm).await.unwrap_err();
        assert!(matches!(
            missing.downcast_ref::<crate::storage::StorageError>(),
            Some(crate::storage::StorageError::NotFound(_))
        ));
        let top = top_used(&group, 3, UsageSort::Returned).await.unwrap();
        assert_eq!(top[0].memory_id, gm);
    }

    /// A deleted memory still ranks in top-N, without a summary.
    #[tokio::test]
    async fn top_used_keeps_a_deleted_memory_without_its_summary() {
        let (_t, store) = store().await;
        let id = crate::ops::create_memory(&store, create_params("gone"), None, Surface::Cli)
            .await
            .unwrap()
            .id;
        record_fetched(&store, &id, Surface::Cli, None).await;
        crate::ops::delete_memory(&store, &id, Surface::Cli)
            .await
            .unwrap();
        let top = top_used(&store, 3, UsageSort::Fetched).await.unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(
            (top[0].memory_id.as_str(), top[0].summary.as_deref()),
            (id.as_str(), None)
        );
    }
}
