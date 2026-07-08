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

use engramdb::daemon::{DaemonCell, DaemonPolicy};
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
