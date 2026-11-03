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

    /// Every indexed memory, invalidated ones included (the caller filters).
    pub async fn list(&self) -> Result<Vec<IndexFilterable>, ApiError> {
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

    pub async fn get(&self, id: &str) -> Result<Memory, ApiError> {
        self.inner.get(id).await.map_err(ApiError::from_storage)
    }

    /// The memories among `ids` that exist; the rest are left out.
    pub async fn get_batch(&self, ids: &[String]) -> Result<Vec<(String, Memory)>, ApiError> {
        self.inner
            .get_batch(ids)
            .await
            .map_err(ApiError::from_storage)
    }

    pub async fn stats(&self) -> Result<ops::StoreStats, ApiError> {
        ops::compute_stats(&self.inner)
            .await
            .map_err(ApiError::from_anyhow)
    }

    pub async fn doctor(&self) -> Result<ops::DoctorResult, ApiError> {
        ops::doctor(&self.inner)
            .await
            .map_err(ApiError::from_anyhow)
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

    /// Exact usage of one memory (a full id).
    pub async fn usage_of(&self, memory_id: &str) -> Result<UsageView, ApiError> {
        ops::usage::load_usage(self.root(), self.store_id(), Some(memory_id))
            .await
            .map_err(ApiError::internal)
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

    /// How many stores are open, for tests.
    pub async fn open_count(&self) -> usize {
        self.entries.lock().await.len()
    }
}
