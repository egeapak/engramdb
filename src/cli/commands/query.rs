//! Unified memory query command.

use crate::cli::output::OutputFormatter;
use crate::ops::{parse_detail_level, parse_memory_type, validate_score};
use crate::retrieval::engine::{RetrievalMode, RetrievalQuery};
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

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
    embedding_backend: Option<crate::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path, embedding_backend).await;

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
        crate::retrieval::engine::DetailLevel::Content
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

    let mut result = crate::ops::query_memories(&engine, &query).await?;

    // Optionally fold in global-store memories. Mirrors the MCP
    // `include_global` option; skipped when already querying the global
    // store (`--global`) since there is nothing extra to merge.
    if params.include_global && !global {
        if let Ok(global_store) = MemoryStore::open_global().await {
            let global_config = global_store
                .project_dir
                .join(".engramdb")
                .join("config.toml");
            let global_engine =
                crate::ops::build_engine(global_store, &global_config, embedding_backend).await;
            if let Ok(global_result) = crate::ops::query_memories(&global_engine, &query).await {
                let max = query.max_results.unwrap_or(params.max_results);
                crate::ops::merge_scored_memories(
                    &mut result.memories,
                    global_result.memories,
                    max,
                );
                result.total += global_result.total;
            }
        }
    }

    formatter.print_retrieval_result(&result, params.show_scores);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::output::OutputFormatter;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance, Status, Visibility};
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
    async fn test_query_rank_include_global_merges_without_error() {
        // Serialize global-store access and start from a clean global store.
        let _lock = crate::storage::test_support::acquire_global_test_lock().await;
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
        let formatter = OutputFormatter::new(None, false, true);

        // Rank mode with no query text needs no embeddings, so this exercises
        // the global-merge path offline.
        let params = QueryParams {
            mode: RetrievalMode::Rank,
            path: Some("src/main.rs".to_string()),
            include_global: true,
            ..base_params()
        };

        let result = run_query(temp_dir.path(), false, params, None, &formatter).await;
        assert!(result.is_ok(), "include_global query failed: {:?}", result);
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
