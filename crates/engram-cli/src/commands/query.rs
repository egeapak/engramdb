//! Unified memory query command.

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::ops::{
    parse_detail_level, parse_memory_type, validate_score, DaemonCell, DaemonPolicy,
};
use engramdb::retrieval::engine::{RetrievalMode, RetrievalQuery, RetrievalResult};
use engramdb::storage::MemoryStore;
use std::path::Path;
use std::sync::Arc;

/// Parameters for the query command.
pub struct QueryParams {
    pub mode: RetrievalMode,
    pub query: Option<String>,
    pub path: Option<String>,
    pub logical: Vec<String>,
    pub type_filter: Vec<String>,
    pub tags: Vec<String>,
    pub min_criticality: Option<f64>,
    pub max_results: usize,
    pub detail_level: Option<String>,
    pub include_expired: bool,
    pub show_scores: bool,
    /// Also merge global-store memories into the project results.
    pub include_global: bool,
}

/// Run a unified retrieval query.
///
/// `mode: Rank` behaves like the old `retrieve` flow — returns every
/// memory passing the type/tag/criticality/physical filters, scored and
/// sorted. `mode: Filter` requires at least one positive relevance signal
/// (keyword, semantic, scope proximity, or tag match).
pub async fn run_query(
    dir: &Path,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    run_query_with_cell(
        dir,
        global,
        params,
        embedding_backend,
        formatter,
        None,
        DaemonPolicy::InProcess,
    )
    .await
}

/// Like [`run_query`] but routes model resolution through the shared daemon cell.
pub async fn run_query_with_daemon(
    dir: &Path,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    run_query_with_cell(
        dir,
        global,
        params,
        embedding_backend,
        formatter,
        Some(cell),
        policy,
    )
    .await
}

async fn run_query_with_cell(
    dir: &Path,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: Option<&Arc<DaemonCell>>,
    policy: DaemonPolicy,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }

    let show_scores = params.show_scores;
    let result =
        compute_query_result_with_cell(store, global, params, embedding_backend, cell, policy)
            .await?;

    formatter.print_retrieval_result(&result, show_scores);

    Ok(())
}

/// Build the retrieval engine for `store`, run `params` against it, and
/// (when `params.include_global` is set and we are not already querying the
/// global store) merge in global-store hits — mirroring the MCP
/// `include_global` option. Returned separately from rendering so the merge
/// behavior is unit-testable offline.
///
/// Used from tests; production callers go through [`compute_query_result_with_cell`].
#[cfg(test)]
pub(crate) async fn compute_query_result(
    store: MemoryStore,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
) -> Result<RetrievalResult> {
    compute_query_result_with_cell(
        store,
        global,
        params,
        embedding_backend,
        None,
        DaemonPolicy::InProcess,
    )
    .await
}

async fn compute_query_result_with_cell(
    store: MemoryStore,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    cell: Option<&Arc<DaemonCell>>,
    policy: DaemonPolicy,
) -> Result<RetrievalResult> {
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let engine = if let Some(c) = cell {
        let config = engramdb::storage::config::load_config(&config_path)
            .await
            .unwrap_or_default();
        let project_dir = store.project_dir.clone();
        let providers =
            engramdb::ops::resolve_providers(c, &config, embedding_backend, &project_dir, policy)
                .await;
        engramdb::ops::assemble_engine(store, config, providers)
    } else {
        engramdb::ops::build_engine(store, &config_path, embedding_backend).await
    };

    let types = if !params.type_filter.is_empty() {
        let parsed_types: Result<Vec<_>> = params
            .type_filter
            .iter()
            .map(|s| parse_memory_type(s))
            .collect();
        Some(parsed_types?)
    } else {
        None
    };

    let tags = if !params.tags.is_empty() {
        Some(params.tags)
    } else {
        None
    };

    let detail_level = if let Some(ref level_str) = params.detail_level {
        parse_detail_level(level_str)?
    } else {
        engramdb::retrieval::engine::DetailLevel::Content
    };

    if let Some(mc) = params.min_criticality {
        validate_score(mc, "min_criticality")?;
    }

    let query = RetrievalQuery {
        mode: params.mode,
        path: params.path,
        logical: params.logical,
        query: params.query,
        types,
        tags,
        min_criticality: params.min_criticality,
        max_results: Some(params.max_results),
        include_expired: Some(params.include_expired),
        detail_level,
    };

    let mut result = engramdb::ops::query_memories(&engine, &query).await?;

    // Optionally fold in global-store memories. Skipped when already
    // querying the global store (`--global`) — nothing extra to merge.
    if params.include_global && !global {
        if let Ok(global_store) = MemoryStore::open_global().await {
            let global_config_path = global_store
                .project_dir
                .join(".engramdb")
                .join("config.toml");
            let global_engine = if let Some(c) = cell {
                let gcfg = engramdb::storage::config::load_config(&global_config_path)
                    .await
                    .unwrap_or_default();
                let gdir = global_store.project_dir.clone();
                let gproviders =
                    engramdb::ops::resolve_providers(c, &gcfg, embedding_backend, &gdir, policy)
                        .await;
                engramdb::ops::assemble_engine(global_store, gcfg, gproviders)
            } else {
                engramdb::ops::build_engine(global_store, &global_config_path, embedding_backend)
                    .await
            };
            if let Ok(global_result) = engramdb::ops::query_memories(&global_engine, &query).await {
                let max = query.max_results.unwrap_or(params.max_results);
                engramdb::ops::merge_scored_memories(
                    &mut result.memories,
                    global_result.memories,
                    max,
                );
                result.total += global_result.total;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputFormatter;
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::types::{Memory, MemoryType, Provenance, Status, Visibility};
    use tempfile::TempDir;

    fn create_test_memory(id: &str, type_: MemoryType, criticality: f64) -> Memory {
        Memory {
            id: id.to_string(),
            type_,
            summary: format!("Test summary for {}", id),
            title: None,
            content: format!("Test content for {}", id),
            details: None,
            physical: vec!["src/main.rs".to_string()],
            logical: vec!["app.core".to_string()],
            tags: vec![],
            criticality,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.9,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            accessed_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    async fn setup_test_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem1 = create_test_memory("mem-test-001-aaa", MemoryType::Decision, 0.9);
        let mem2 = create_test_memory("mem-test-002-bbb", MemoryType::Hazard, 0.7);
        let mem3 = create_test_memory("mem-test-003-ccc", MemoryType::Convention, 0.3);

        store.create(&mem1).await.unwrap();
        store.create(&mem2).await.unwrap();
        store.create(&mem3).await.unwrap();

        (temp_dir, store)
    }

    fn base_params() -> QueryParams {
        QueryParams {
            mode: RetrievalMode::Rank,
            query: None,
            path: None,
            logical: vec![],
            type_filter: vec![],
            tags: vec![],
            min_criticality: None,
            max_results: 10,
            detail_level: None,
            include_expired: false,
            show_scores: false,
            include_global: false,
        }
    }

    #[tokio::test]
    async fn test_query_rank_returns_ok() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            path: Some("src/main.rs".to_string()),
            ..base_params()
        };

        let result = run_query(temp_dir.path(), false, params, None, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_query_rank_invalid_detail_level() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            path: Some("src/main.rs".to_string()),
            detail_level: Some("invalid".to_string()),
            ..base_params()
        };

        let result = run_query(temp_dir.path(), false, params, None, &formatter).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid detail level"));
    }

    #[tokio::test]
    async fn test_query_rank_with_type_filter() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            path: Some("src/main.rs".to_string()),
            type_filter: vec!["decision".to_string()],
            ..base_params()
        };

        let result = run_query(temp_dir.path(), false, params, None, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_query_filter_with_text() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            mode: RetrievalMode::Filter,
            query: Some("Test".to_string()),
            ..base_params()
        };

        let result = run_query(temp_dir.path(), false, params, None, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_include_global_merges_global_hits_with_negative_control() {
        // Serialize global-store access and start from a clean global store.
        let _lock = engramdb::storage::test_support::acquire_global_test_lock().await;
        let global = MemoryStore::open_global().await.unwrap();
        global
            .create(&create_test_memory(
                "global-q-001-zzz",
                MemoryType::Decision,
                0.95,
            ))
            .await
            .unwrap();

        let (temp_dir, _store) = setup_test_store().await;

        // Rank mode with no query text needs no embeddings → fully offline.
        let mk = |include_global: bool| QueryParams {
            mode: RetrievalMode::Rank,
            path: Some("src/main.rs".to_string()),
            include_global,
            ..base_params()
        };

        // include_global = true → the global memory is folded in.
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let merged = compute_query_result(store, false, mk(true), None)
            .await
            .unwrap();
        assert!(
            merged
                .memories
                .iter()
                .any(|m| m.memory.id == "global-q-001-zzz"),
            "global memory should be merged when include_global=true"
        );

        // include_global = false → negative control: it must NOT appear.
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let project_only = compute_query_result(store, false, mk(false), None)
            .await
            .unwrap();
        assert!(
            !project_only
                .memories
                .iter()
                .any(|m| m.memory.id == "global-q-001-zzz"),
            "global memory must NOT appear when include_global=false"
        );
    }

    #[tokio::test]
    async fn test_query_filter_requires_signal() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        // No query, logical, path, or tags — must error.
        let params = QueryParams {
            mode: RetrievalMode::Filter,
            min_criticality: Some(0.5),
            ..base_params()
        };

        let result = run_query(temp_dir.path(), false, params, None, &formatter).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("filter requires at least one"),
            "expected validation message, got: {}",
            msg
        );
    }
}
