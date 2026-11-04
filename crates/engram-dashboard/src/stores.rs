//! Read-only store handles and a small LRU of them.
//!
//! [`ReadStore`] is the only way a handler reaches a store, and it exposes
//! reads only: the `MemoryStore` inside is private to this module, so "no
//! writes on GET" is a property of the type rather than of reviewer
//! attention. None of these reads records a usage event.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

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

impl ReadStore {
    async fn open(location: &Location) -> Result<Self, ApiError> {
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

    pub async fn doctor(&self) -> Result<ops::DoctorResult, ApiError> {
        match ops::doctor(&self.inner).await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.index_error(ApiError::from_anyhow(e)).await),
        }
    }

    /// The tiered drift check every `query` runs, as a warning string.
    pub async fn staleness(&self) -> Option<String> {
        let cfg = self.config().await;
        self.inner.check_staleness(&cfg.index).await.ok().flatten()
    }

    pub async fn schema_state(&self) -> SchemaState {
        self.inner.schema_state().await
    }

    pub async fn checkout_conflict(&self) -> Option<PathBuf> {
        self.inner.checkout_conflict().await
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
    /// (the page says which are dangling, invalidated or rejected). A
    /// cross-store far end is checked by opening that store read-only.
    pub async fn links_of(&self, id: &str) -> Result<ops::links::LinksView, ApiError> {
        match ops::links::links_of(&self.inner, id, &ops::links::LinkFilter::all()).await {
            Ok(v) => Ok(v),
            Err(e) => Err(self.index_error(ApiError::from_anyhow(e)).await),
        }
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
