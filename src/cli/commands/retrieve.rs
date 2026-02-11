//! Retrieve memories by context.

use crate::cli::output::OutputFormatter;
use crate::ops::parse_memory_type;
use crate::retrieval::engine::{DetailLevel, RetrievalQuery};
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Parameters for the retrieve command.
pub struct RetrieveParams {
    pub path: Option<String>,
    pub logical: Vec<String>,
    pub query: Option<String>,
    pub type_filter: Vec<String>,
    pub tags: Vec<String>,
    pub min_criticality: Option<f64>,
    pub max_results: usize,
    pub detail_level: Option<String>,
    pub include_expired: bool,
    pub show_scores: bool,
}

/// Retrieve memories based on context and query.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `params` - Retrieval query parameters
/// * `formatter` - Output formatter for displaying results
pub async fn run_retrieve(
    dir: &Path,
    registry: &dyn RegistryBackend,
    params: RetrieveParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path).await;

    // Parse type filters
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

    // Parse tags
    let tags = if !params.tags.is_empty() {
        Some(params.tags)
    } else {
        None
    };

    // Parse detail_level
    let detail_level = if let Some(ref level_str) = params.detail_level {
        match level_str.to_lowercase().as_str() {
            "summary" => DetailLevel::Summary,
            "content" => DetailLevel::Content,
            "full" => DetailLevel::Full,
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid detail level: {}. Must be summary, content, or full",
                    level_str
                ))
            }
        }
    } else {
        DetailLevel::Content
    };

    // Build retrieval query
    let query = RetrievalQuery {
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

    // Perform retrieval
    let result = crate::ops::retrieve_memories(&engine, &query).await?;

    // Display results
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

    async fn setup_test_store() -> (TempDir, MemoryStore, InMemoryRegistry) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem1 = create_test_memory("mem-test-001-aaa", MemoryType::Decision, 0.9);
        let mem2 = create_test_memory("mem-test-002-bbb", MemoryType::Hazard, 0.7);
        let mem3 = create_test_memory("mem-test-003-ccc", MemoryType::Convention, 0.3);

        store.create(&mem1).await.unwrap();
        store.create(&mem2).await.unwrap();
        store.create(&mem3).await.unwrap();

        (temp_dir, store, registry)
    }

    #[tokio::test]
    async fn test_retrieve_returns_ok() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = RetrieveParams {
            path: Some("src/main.rs".to_string()),
            logical: vec![],
            query: None,
            type_filter: vec![],
            tags: vec![],
            min_criticality: None,
            max_results: 10,
            detail_level: None,
            include_expired: false,
            show_scores: false,
        };

        let result = run_retrieve(temp_dir.path(), &registry, params, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_retrieve_invalid_detail_level() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = RetrieveParams {
            path: Some("src/main.rs".to_string()),
            logical: vec![],
            query: None,
            type_filter: vec![],
            tags: vec![],
            min_criticality: None,
            max_results: 10,
            detail_level: Some("invalid".to_string()),
            include_expired: false,
            show_scores: false,
        };

        let result = run_retrieve(temp_dir.path(), &registry, params, &formatter).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid detail level"));
    }

    #[tokio::test]
    async fn test_retrieve_with_type_filter() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = RetrieveParams {
            path: Some("src/main.rs".to_string()),
            logical: vec![],
            query: None,
            type_filter: vec!["decision".to_string()],
            tags: vec![],
            min_criticality: None,
            max_results: 10,
            detail_level: None,
            include_expired: false,
            show_scores: false,
        };

        let result = run_retrieve(temp_dir.path(), &registry, params, &formatter).await;
        assert!(result.is_ok());
    }
}
