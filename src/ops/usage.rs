//! Per-memory usage recording and reads — the policy layer over
//! `engram_storage::telemetry::{usage, memory_events}`.
//!
//! **Where recording happens.** Every write op in this module tree (`create`,
//! `update`, `delete`, `challenge`, `resolve`, `verify`, `compress`,
//! consolidation, promotion) takes a [`Surface`] and records its own event, so
//! the CLI and the MCP server — which both pass through here — never repeat
//! it. The three read signals are recorded where only a front end knows what
//! the caller saw: `returned` after a CLI or MCP `query` (never inside the
//! engine, which hooks and maintenance also drive), `fetched` after a CLI or
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
    load_daily_series, load_rollup, load_usage, top_memories, DailyUsage, MemoryEvent, Surface,
    UsageCounts, UsageKind, UsageSort, UsageView, DEFAULT_SERIES_DAYS,
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
    crate::storage::config::load_config_or_default(
        &store.project_dir.join(".engramdb").join("config.toml"),
    )
    .await
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
    // here, behind the gate, so a disabled store pays nothing.
    let full = store
        .resolve_id(id)
        .await
        .unwrap_or_else(|_| id.to_string());
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
/// `retention_days` (folding them into the rollup first) and compact.
/// Best-effort: failures are logged.
pub async fn maintain_store_usage(store: &StoreRef, enabled: bool, retention_days: Option<u64>) {
    if enabled {
        if let Err(e) = crate::telemetry::usage::drain_to_table(&store.root, &store.id).await {
            tracing::debug!(store = %store.id, "usage stats: drain failed (non-fatal): {e}");
        }
    }
    if let Some(days) = retention_days {
        if let Err(e) = crate::telemetry::usage::prune_memory_events(&store.id, days).await {
            tracing::debug!(store = %store.id, "usage stats: prune failed (non-fatal): {e}");
        }
    }
    if let Err(e) = crate::telemetry::usage::optimize_memory_events(&store.id).await {
        tracing::debug!(store = %store.id, "usage stats: optimize failed (non-fatal): {e}");
    }
}

/// Usage housekeeping for a project: its own log, plus the logs of every
/// shared store its queries fan into (subscribed groups and the global
/// store). A `returned` event for a group memory is written to the group's
/// log, and nothing else drains a group store, so the subscribers do it.
/// Shared stores use the project's `[stats]` settings: the project is the one
/// running the pass. `gc` and the throttled CLI maintenance pass call this;
/// the daemon calls [`maintain_project_usage`] as it serves hooks.
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
        maintain_store_usage(&other, enabled, retention_days).await;
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
pub async fn memory_usage_report(
    store: &MemoryStore,
    id: &str,
) -> anyhow::Result<MemoryUsageReport> {
    let (owner, memory) = match store.get(id).await {
        Ok(m) => (StoreRef::of(store), m),
        Err(crate::storage::StorageError::NotFound(_))
            if !(store.is_group() || store.is_global()) =>
        {
            let mut found = None;
            for shared in shared_stores_of(&store.project_id).await {
                let opened = if shared.id == crate::storage::paths::GLOBAL_PROJECT_ID {
                    MemoryStore::open_global().await
                } else {
                    MemoryStore::open_group(&shared.id).await
                };
                if let Ok(s) = opened {
                    if let Ok(m) = s.get(id).await {
                        found = Some((shared, m));
                        break;
                    }
                }
            }
            found.ok_or_else(|| crate::storage::StorageError::NotFound(id.to_string()))?
        }
        Err(e) => return Err(e.into()),
    };
    let view = load_usage(&owner.root, &owner.id, Some(&memory.id)).await;
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
    .await;
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

/// The store's `n` most-used memories by `sort`, with their summaries.
pub async fn top_used(store: &MemoryStore, n: usize, sort: UsageSort) -> Vec<TopMemory> {
    let view = load_usage(&store.project_dir, &store.project_id, None).await;
    let top = top_memories(&view, n, sort);
    let ids: Vec<&str> = top.iter().map(|u| u.memory_id.as_str()).collect();
    let summaries: HashMap<String, String> = match store.get_batch(&ids).await {
        Ok(found) => found.into_iter().map(|(id, m)| (id, m.summary)).collect(),
        Err(_) => HashMap::new(),
    };
    top.into_iter()
        .map(|u| TopMemory {
            summary: summaries.get(&u.memory_id).cloned(),
            memory_id: u.memory_id,
            counts: u.counts,
        })
        .collect()
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

        let top = top_used(&store, 5, UsageSort::Fetched).await;
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
}
