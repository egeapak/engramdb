//! Search memories by keyword.

use crate::cli::output::OutputFormatter;
use crate::ops::parse_memory_type;
use crate::retrieval::filters::SearchFilters;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Parameters for the search command.
pub struct SearchParams {
    pub query: String,
    pub type_filter: Vec<String>,
    pub tags: Vec<String>,
    pub physical: Option<String>,
    pub logical: Vec<String>,
    pub min_criticality: Option<f64>,
    pub max_results: usize,
}

/// Search memories by keyword.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `params` - Search query parameters
/// * `formatter` - Output formatter for displaying results
pub async fn run_search(
    dir: &Path,
    registry: &dyn RegistryBackend,
    params: SearchParams,
    embedding_backend: Option<crate::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path, embedding_backend).await;

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

    // Build search filters
    let filters = SearchFilters {
        types,
        tags,
        physical: params.physical,
        logical: params.logical.first().cloned(),
        min_criticality: params.min_criticality,
    };

    // Perform search
    let mut results = crate::ops::search_memories(&engine, &params.query, &filters).await?;

    // Apply max_results limit
    results.truncate(params.max_results);

    // Display results
    formatter.print_search_results(&results);

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
    async fn test_search_returns_ok() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = SearchParams {
            query: "Test".to_string(),
            type_filter: vec![],
            tags: vec![],
            physical: None,
            logical: vec![],
            min_criticality: None,
            max_results: 10,
        };

        let result = run_search(temp_dir.path(), &registry, params, None, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_with_type_filter() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = SearchParams {
            query: "Test".to_string(),
            type_filter: vec!["decision".to_string()],
            tags: vec![],
            physical: None,
            logical: vec![],
            min_criticality: None,
            max_results: 10,
        };

        let result = run_search(temp_dir.path(), &registry, params, None, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_max_results() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = SearchParams {
            query: "Test".to_string(),
            type_filter: vec![],
            tags: vec![],
            physical: None,
            logical: vec![],
            min_criticality: None,
            max_results: 1,
        };

        let result = run_search(temp_dir.path(), &registry, params, None, &formatter).await;
        assert!(result.is_ok());
    }
}
