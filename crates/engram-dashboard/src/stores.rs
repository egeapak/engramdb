//! Read-only store handles and a small LRU of them.
//!
//! [`ReadStore`] is the only way a handler reaches a store, and it exposes
//! reads only: the `MemoryStore` inside is private to this module, so "no
//! writes on GET" is a property of the type rather than of reviewer
//! attention. None of these reads records a usage event.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engramdb::ops;
use engramdb::ops::usage::{DailyUsage, UsageView};
use engramdb::storage::lance_index::IndexFilterable;
use engramdb::storage::{MemoryStore, SchemaState, StorageError};
use engramdb::types::{EngramConfig, Memory};
use tokio::sync::Mutex;

use crate::projects::{Location, StoreKind};
use crate::ApiError;

/// Open stores kept at once. Each holds a LanceDB connection; the dashboard
/// is usually looking at one project and occasionally switching.
pub const DEFAULT_CAPACITY: usize = 8;

/// A store opened with `open_read_only`, exposing reads only.
pub struct ReadStore {
    inner: MemoryStore,
    location: Location,
}

/// How long opening a store (a directory stat, the project id's `.git`
/// reads, the LanceDB connect) may take before the request is a 409.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a whole-store read that hashes every file (`doctor`, the
/// staleness check at its `content` tier) may take.
pub const SLOW_READ_TIMEOUT: Duration = Duration::from_secs(60);

impl ReadStore {
    /// Opened off the runtime workers ([`crate::projects::off_runtime`],
    /// keyed by the location): `open_read_only` stats the directory and
    /// computes the project id with synchronous reads, which on a hung
    /// mount would pin a worker where no timeout could fire.
    async fn open(location: &Location) -> Result<Self, ApiError> {
        let loc = location.clone();
        let key = format!("open:{}:{}", loc.id, loc.root.display());
        match crate::projects::off_runtime(key, OPEN_TIMEOUT, Self::open_here(loc)).await {
            Ok(r) => r,
            Err(crate::projects::Refused::Failed) => {
                Err(ApiError::internal("opening the store failed"))
            }
            Err(e) => Err(ApiError::unavailable(format!(
                "opening the store in {} {e} (an unreachable mount?)",
                location.root.display()
            ))),
        }
    }

    async fn open_here(location: Location) -> Result<Self, ApiError> {
        let location = &location;
        let opened = match location.kind {
            StoreKind::Global => MemoryStore::open_global_read_only().await,
            StoreKind::Group => MemoryStore::open_group_read_only(&location.id).await,
            StoreKind::Project => MemoryStore::open_read_only(&location.root).await,
        };
        let inner = opened.map_err(|e| match e {
            StorageError::NotInitialized => {
                ApiError::unavailable(format!("{} has no EngramDB store", location.root.display()))
            }
            StorageError::Validation(msg) => ApiError::unavailable(msg),
            other => ApiError::internal(other),
        })?;
        Ok(Self {
            inner,
            location: location.clone(),
        })
    }

    /// The id the store's own data is keyed by (its LanceDB directory and
    /// usage tables), which is what usage reads need.
    pub fn store_id(&self) -> &str {
        &self.inner.project_id
    }

    pub fn root(&self) -> &Path {
        &self.inner.project_dir
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub async fn config(&self) -> EngramConfig {
        engramdb::storage::config::load_config_or_default(
            &engramdb::storage::paths::project_dir(self.root()).join("config.toml"),
        )
        .await
    }

    /// Map a failed *index* read. The storage layer reports every LanceDB
    /// failure as `Validation`, which would read as the caller's fault (400).
    /// A store whose schema is behind this binary is the usual cause — the
    /// dashboard does not migrate (it opens read-only), so a pre-0.3.0 table
    /// is missing columns this binary selects — and that is a 409 naming the
    /// fix; anything else is a 500. A 404 passes through.
    async fn index_error(&self, e: ApiError) -> ApiError {
        if e.status == axum::http::StatusCode::NOT_FOUND {
            return e;
        }
        let schema = self.schema_state().await;
        if schema.needs_migration {
            return ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "schema_behind",
                format!(
                    "this store's index is on schema {}, older than this binary's {}; the \
                     dashboard does not upgrade stores. Run `engramdb reindex` (or any \
                     engramdb command) in {} to upgrade it. ({})",
                    schema.stored.as_deref().unwrap_or("unknown"),
                    schema.current,
                    self.root().display(),
                    e.message
                ),
            );
        }
        if e.status == axum::http::StatusCode::BAD_REQUEST {
            return ApiError::internal(format!("cannot read the index: {}", e.message));
        }
        e
    }

    /// The memory count the manifest records (kept current by every write),
    /// to tell an empty store from one whose index is being rebuilt.
    pub async fn manifest_memory_count(&self) -> Option<usize> {
        engramdb::storage::manifest::load_manifest(
            &engramdb::storage::paths::project_dir(self.root()).join("manifest.toml"),
        )
        .await
        .ok()
        .map(|m| m.stats.memory_count)
    }

    /// Every indexed memory, invalidated ones included (the caller filters).
    pub async fn list(&self) -> Result<Vec<IndexFilterable>, ApiError> {
        match self.list_raw().await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.index_error(e).await),
        }
    }

    async fn list_raw(&self) -> Result<Vec<IndexFilterable>, ApiError> {
        let params = ops::ListParams {
            types: None,
            epistemic: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: ops::SortField::Created,
            reverse: false,
            limit: None,
            include_invalidated: true,
        };
        ops::list_memories(&self.inner, &params)
            .await
            .map_err(ApiError::from_anyhow)
    }

    /// One memory by id or unique prefix. An ambiguous prefix is the one
    /// `Validation` here that is the caller's (400); see [`Self::index_error`].
    pub async fn get(&self, id: &str) -> Result<Memory, ApiError> {
        match self.inner.get(id).await {
            Ok(m) => Ok(m),
            Err(StorageError::Validation(msg)) if msg.starts_with("Ambiguous ID prefix") => Err(
                ApiError::new(axum::http::StatusCode::BAD_REQUEST, "ambiguous_id", msg),
            ),
            Err(e) => Err(self.index_error(ApiError::from_storage(e)).await),
        }
    }

    /// The memories among `ids` that exist; the rest are left out.
    pub async fn get_batch(&self, ids: &[String]) -> Result<Vec<(String, Memory)>, ApiError> {
        match self.inner.get_batch(ids).await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.index_error(ApiError::from_storage(e)).await),
        }
    }

    pub async fn stats(&self) -> Result<ops::StoreStats, ApiError> {
        match ops::compute_stats(&self.inner).await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.index_error(ApiError::from_anyhow(e)).await),
        }
    }

    /// Off the runtime workers (`doctor` canonicalizes and stats the
    /// registered checkout's path, which may be on another mount).
    pub async fn doctor(&self) -> Result<ops::DoctorResult, ApiError> {
        let store = self.inner.clone();
        let ran = crate::projects::off_runtime(
            format!("doctor:{}", self.store_id()),
            SLOW_READ_TIMEOUT,
            async move { ops::doctor(&store).await },
        )
        .await;
        match ran {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(self.index_error(ApiError::from_anyhow(e)).await),
            Err(crate::projects::Refused::Failed) => Err(ApiError::internal("doctor failed")),
            Err(e) => Err(ApiError::unavailable(format!("doctor {e}"))),
        }
    }

    /// The tiered drift check every `query` runs, as a warning string. Off
    /// the runtime workers, like [`Self::doctor`]; one that does not finish
    /// says so rather than reading as "fresh".
    pub async fn staleness(&self) -> Option<String> {
        let cfg = self.config().await;
        let store = self.inner.clone();
        let ran = crate::projects::off_runtime(
            format!("staleness:{}", self.store_id()),
            SLOW_READ_TIMEOUT,
            async move { store.check_staleness(&cfg.index).await.ok().flatten() },
        )
        .await;
        match ran {
            Ok(v) => v,
            Err(crate::projects::Refused::Failed) => None,
            Err(e) => Some(format!("the index freshness check {e}")),
        }
    }

    pub async fn schema_state(&self) -> SchemaState {
        self.inner.schema_state().await
    }

    /// Off the runtime workers and bounded: it canonicalizes and stats the
    /// *other* checkout's path, which may be on a dead mount. One that does
    /// not answer in time is `None`, as an unreadable registry already is.
    pub async fn checkout_conflict(&self) -> Option<PathBuf> {
        let store = self.inner.clone();
        crate::projects::off_runtime(
            format!("conflict:{}", self.store_id()),
            crate::projects::ROW_TIMEOUT,
            async move { store.checkout_conflict().await },
        )
        .await
        .ok()
        .flatten()
    }

    /// Usage of every memory: the rollup plus the undrained log.
    pub async fn usage_all(&self) -> Result<UsageView, ApiError> {
        ops::usage::load_current_usage(self.root(), self.store_id())
            .await
            .map_err(ApiError::internal)
    }

    /// Exact usage of one memory (a full id): a raw scan of its events. The
    /// list uses [`Self::usage_all`] (the rollup plus the log), which can lag
    /// this by the events a rollup refresh has not folded in yet, and whose
    /// session counts are an upper bound; the detail page is the exact one.
    pub async fn usage_of(&self, memory_id: &str) -> Result<UsageView, ApiError> {
        ops::usage::load_usage(self.root(), self.store_id(), Some(memory_id))
            .await
            .map_err(ApiError::internal)
    }

    /// The store's edges and node set ([`ops::links::graph_of`], i.e.
    /// `load_graph`) over memory rows already read with [`Self::list`]
    /// (invalidated included, as `list` returns them), so a handler that
    /// needs the rows anyway reads the memories table once. Reads the index
    /// only and creates nothing: a store without an `edges` table (not yet
    /// on schema 0.9.0) answers with no edges.
    pub async fn graph_of(
        &self,
        rows: &[IndexFilterable],
        filter: &ops::links::GraphFilter,
    ) -> Result<ops::links::StoreGraph, ApiError> {
        Ok(ops::links::graph_of(rows, self.edges().await?, filter))
    }

    /// Every edge of the store, deduplicated (one row per logical edge).
    pub async fn edges(&self) -> Result<Vec<ops::links::EdgeRow>, ApiError> {
        match self.inner.list_edges().await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.index_error(ApiError::from_storage(e)).await),
        }
    }

    /// One memory's edges in both directions, every state and status shown
    /// (the page says which are dangling, invalidated or rejected): what
    /// `ops::links::links_of` with `LinkFilter::all()` answers, except that
    /// a cross-store far end is checked through the dashboard's bounded
    /// prober ([`crate::graph::probe_stores`]: off the runtime workers,
    /// concurrently, under `ROW_TIMEOUT` each, at most 8 stores, one probe
    /// per store in flight) rather than `links_of`'s serial, unbounded opens
    /// on the calling worker. An unprobed or timed-out store is `unknown`.
    pub async fn links_of(&self, id: &str) -> Result<ops::links::LinksView, ApiError> {
        use ops::links::{Direction, EdgeState, EdgeView, LinksView};
        let storage = |e| ApiError::from_storage(e);
        let full = match self.inner.resolve_id(id).await {
            Ok(f) => f,
            Err(e) => return Err(self.index_error(storage(e)).await),
        };
        let rows = match self.inner.edges_touching(&full).await {
            Ok(r) => r,
            Err(e) => return Err(self.index_error(storage(e)).await),
        };
        // The far end: an inbound edge's is always in this store (the table
        // holds only edges this store's files declare).
        let other = |r: &ops::links::EdgeRow| {
            if r.src_id == full {
                (r.dst_id.clone(), r.dst_store.clone())
            } else {
                (r.src_id.clone(), None)
            }
        };
        let mut local_ids: Vec<String> = Vec::new();
        let mut by_store: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for r in &rows {
            match other(r) {
                (oid, None) => local_ids.push(oid),
                (oid, Some(sid)) => by_store.entry(sid).or_default().push(oid),
            }
        }
        let local: std::collections::HashMap<String, Memory> =
            match self.inner.get_batch(&local_ids).await {
                Ok(v) => v.into_iter().collect(),
                Err(e) => return Err(self.index_error(storage(e)).await),
            };
        let foreign = crate::graph::probe_stores(by_store).await;
        let mut view = LinksView {
            id: full.clone(),
            outbound: Vec::new(),
            inbound: Vec::new(),
            hidden: 0,
        };
        for row in rows {
            let out = row.src_id == full;
            let (other_id, other_store) = other(&row);
            let (state, summary, successor) = match &other_store {
                None => match local.get(&other_id) {
                    Some(m) if m.is_invalidated() => (
                        EdgeState::Invalidated,
                        Some(m.summary.clone()),
                        m.superseded_by.clone(),
                    ),
                    Some(m) => (EdgeState::Live, Some(m.summary.clone()), None),
                    None => (EdgeState::Dangling, None, None),
                },
                Some(sid) => match foreign.get(sid) {
                    Some(Some(found)) if found.contains(&other_id) => (EdgeState::Live, None, None),
                    Some(Some(_)) => (EdgeState::Dangling, None, None),
                    _ => (EdgeState::Unknown, None, None),
                },
            };
            let edge = EdgeView {
                direction: if out { Direction::Out } else { Direction::In },
                other_id,
                other_store,
                other_summary: summary,
                symmetric: row.kind.is_symmetric(),
                kind: row.kind,
                confidence: row.confidence,
                origin: row.origin,
                status: row.status,
                created_at: row.created_at,
                note: row.note,
                state,
                successor,
            };
            if out {
                view.outbound.push(edge);
            } else {
                view.inbound.push(edge);
            }
        }
        let order = |a: &EdgeView, b: &EdgeView| {
            (a.kind.as_str(), &a.other_id).cmp(&(b.kind.as_str(), &b.other_id))
        };
        view.outbound.sort_by(order);
        view.inbound.sort_by(order);
        Ok(view)
    }

    /// The daily series of the last 30 days, for one memory or the store.
    pub async fn series(&self, memory_id: Option<&str>) -> Result<Vec<DailyUsage>, ApiError> {
        ops::usage::load_daily_series(
            self.root(),
            self.store_id(),
            memory_id,
            ops::usage::DEFAULT_SERIES_DAYS,
        )
        .await
        .map_err(ApiError::internal)
    }
}

/// Open stores by location, least recently used evicted first.
///
/// Freshness does not depend on the cache: every LanceDB call reopens its
/// table, files are read per request, and the schema state is re-read from
/// the manifest each time. A failed open is never cached, so a store that is
/// indexed (or comes back) after a failure is picked up on the next request.
pub struct StoreCache {
    capacity: usize,
    entries: Mutex<Vec<(Location, Arc<ReadStore>, Instant)>>,
}

impl StoreCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(Vec::new()),
        }
    }

    pub async fn get(&self, location: &Location) -> Result<Arc<ReadStore>, ApiError> {
        {
            let mut entries = self.entries.lock().await;
            if let Some(e) = entries.iter_mut().find(|e| &e.0 == location) {
                e.2 = Instant::now();
                return Ok(e.1.clone());
            }
        }
        // Opened outside the lock: a slow LanceDB connect for one project
        // must not stall every other request.
        let store = Arc::new(ReadStore::open(location).await?);
        let mut entries = self.entries.lock().await;
        if let Some(e) = entries.iter_mut().find(|e| &e.0 == location) {
            // Another request opened it meanwhile; keep theirs.
            e.2 = Instant::now();
            return Ok(e.1.clone());
        }
        if entries.len() >= self.capacity {
            if let Some((i, _)) = entries.iter().enumerate().min_by_key(|(_, e)| e.2) {
                entries.swap_remove(i);
            }
        }
        entries.push((location.clone(), store.clone(), Instant::now()));
        Ok(store)
    }

    /// Drop the cached handle of `location`, so the next read opens it
    /// again. The write path calls this after a write: freshness does not
    /// need it (see above), but a write through `MemoryStore::open` may have
    /// migrated the store under the handle, and a fresh open re-reads it.
    pub async fn invalidate(&self, location: &Location) {
        self.entries.lock().await.retain(|e| &e.0 != location);
    }

    /// How many stores are open, for tests.
    pub async fn open_count(&self) -> usize {
        self.entries.lock().await.len()
    }
}
