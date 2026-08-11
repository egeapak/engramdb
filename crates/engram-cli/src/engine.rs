//! Shared retrieval-engine construction for daemon-aware CLI commands.
//!
//! Every daemon-aware command (`add`, `query`, `update`, `reindex`) needs the
//! same engine: load the store's config, resolve model providers, and assemble
//! a [`RetrievalEngine`]. Provider resolution routes through the shared daemon
//! `cell` when `policy` permits, or loads single-session providers in-process.
//!
//! [`DaemonPolicy::InProcess`] already encodes "no daemon", so this helper
//! always takes a real `&Arc<DaemonCell>` and lets the policy decide. When the
//! policy is `InProcess`, [`engramdb::daemon::resolve_providers`] skips the
//! daemon branch and loads single-session providers in-process — identical to
//! the old `ops::build_engine` path (both call `resolve_engine_providers(config,
//! backend, 1)` then `assemble_engine`).

use engramdb::daemon::{DaemonCell, DaemonPolicy, InProcessFallback};
use engramdb::ops::ProviderCache;
use engramdb::retrieval::engine::RetrievalEngine;
use engramdb::storage::MemoryStore;
use engramdb::types::EmbeddingBackend;
use std::sync::Arc;

/// Build a [`RetrievalEngine`] for `store`, resolving model providers through
/// the shared daemon `cell` per `policy`.
///
/// Consumes `store` — clone it at the call site if you also need the store
/// afterwards (e.g. for the create/update op).
pub async fn engine_for(
    store: MemoryStore,
    backend: Option<EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> RetrievalEngine {
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let config = engramdb::storage::config::load_config_or_default(&config_path).await;
    let project_dir = store.project_dir.clone();
    let providers =
        engramdb::daemon::resolve_providers(cell, &config, backend, &project_dir, policy).await;
    engramdb::ops::assemble_engine(store, config, providers)
}

/// [`engine_for`] for commands that build engines for **several projects in one
/// run** (`projects discover`, `projects repair`).
///
/// Differs only in the in-process fallback: bundles are served from a shared
/// [`ProviderCache`] so the model loads once for the whole batch instead of
/// once per project. The pool is pinned to a single session — `ProviderCache`
/// is the MCP server's seam and otherwise auto-sizes to `cores/2` for many
/// concurrent callers, but these callers are strictly sequential, so the extra
/// sessions could never be used and would cost a full model load each. The
/// size is part of `provider_cache_key`, so this stays a coherent cache key.
pub async fn engine_for_project(
    store: MemoryStore,
    backend: Option<EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
    cache: &ProviderCache,
) -> RetrievalEngine {
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let mut config = engramdb::storage::config::load_config_or_default(&config_path).await;
    config.embeddings.pool_size = Some(1);
    let project_dir = store.project_dir.clone();
    let providers = engramdb::daemon::resolve_providers_with(
        cell,
        &config,
        backend,
        &project_dir,
        policy,
        InProcessFallback::Pool(cache),
    )
    .await;
    engramdb::ops::assemble_engine(store, config, providers)
}
