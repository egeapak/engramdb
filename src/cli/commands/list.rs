//! List all memories with optional filtering.

use crate::cli::output::OutputFormatter;
use crate::ops::{parse_memory_type, parse_status};
use crate::storage::{MemoryStore, RegistryBackend};
use crate::types::MemoryType;
use anyhow::{anyhow, Result};
use std::path::Path;

/// List all memories, optionally filtered by type, tags, status, and scope.
///
/// Returns index entries (lightweight summaries) rather than full memory data.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `type_filter` - Filter by memory types (empty = no filter)
/// * `tags_filter` - Filter by tags, OR logic (empty = no filter)
/// * `status_filter` - Filter by status (None = no filter)
/// * `scope_filter` - Filter by scope (matches physical or logical scopes)
/// * `sort_field` - Sort field: "criticality", "created", "updated", "type"
/// * `reverse` - Reverse sort order
/// * `limit` - Maximum number of results to display
/// * `formatter` - Output formatter for displaying the list
#[allow(clippy::too_many_arguments)]
pub async fn run_list(
    dir: &Path,
    registry: &dyn RegistryBackend,
    type_filter: Vec<String>,
    tags_filter: Vec<String>,
    status_filter: Option<String>,
    scope_filter: Option<String>,
    sort_field: &str,
    reverse: bool,
    limit: Option<usize>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let mut entries = store.list().await?;

    // Apply filters
    if !type_filter.is_empty() {
        let types: Vec<MemoryType> = type_filter
            .iter()
            .map(|s| parse_memory_type(s))
            .collect::<Result<Vec<_>>>()?;
        entries.retain(|e| types.contains(&e.type_));
    }

    if !tags_filter.is_empty() {
        entries.retain(|e| tags_filter.iter().any(|tag| e.tags.contains(tag)));
    }

    if let Some(status_str) = status_filter {
        let status = parse_status(&status_str)?;
        entries.retain(|e| e.status == status);
    }

    // Apply scope filter
    if let Some(scope) = scope_filter {
        entries.retain(|e| {
            e.physical.iter().any(|p| p.contains(&scope))
                || e.logical.iter().any(|l| l.contains(&scope))
        });
    }

    // Apply sorting
    match sort_field {
        "criticality" | "relevance" => {
            entries.sort_by(|a, b| b.criticality.partial_cmp(&a.criticality).unwrap());
        }
        "created" => {
            entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        }
        "updated" => {
            entries.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        }
        "type" => {
            entries.sort_by(|a, b| format!("{:?}", a.type_).cmp(&format!("{:?}", b.type_)));
        }
        _ => {
            return Err(anyhow!(
                "Invalid sort field: {}. Valid options are: criticality, relevance, created, updated, type",
                sort_field
            ));
        }
    }

    // Apply reverse if requested
    if reverse {
        entries.reverse();
    }

    // Apply limit
    if let Some(max) = limit {
        entries.truncate(max);
    }

    formatter.print_memory_list(&entries);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance, Status, Visibility};
    use tempfile::TempDir;

    fn create_test_memory(
        id: &str,
        type_: MemoryType,
        criticality: f64,
        physical: Vec<String>,
        logical: Vec<String>,
    ) -> Memory {
        Memory {
            id: id.to_string(),
            type_,
            summary: format!("Test summary for {}", id),
            content: format!("Test content for {}", id),
            details: None,
            physical,
            logical,
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

        // Create memories with different properties
        let mem1 = create_test_memory(
            "mem1",
            MemoryType::Decision,
            0.9,
            vec!["src/main.rs".to_string()],
            vec!["app.core".to_string()],
        );

        let mem2 = create_test_memory(
            "mem2",
            MemoryType::Hazard,
            0.7,
            vec!["src/lib.rs".to_string()],
            vec!["app.utils".to_string()],
        );

        let mem3 = create_test_memory(
            "mem3",
            MemoryType::Convention,
            0.5,
            vec!["tests/test.rs".to_string()],
            vec!["app.core".to_string()],
        );

        store.create(&mem1).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.create(&mem2).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.create(&mem3).await.unwrap();

        (temp_dir, store)
    }

    #[tokio::test]
    async fn test_scope_filter_physical() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            Some("src/main.rs".to_string()),
            "criticality",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_scope_filter_logical() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            Some("app.core".to_string()),
            "criticality",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_scope_filter_no_match() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            Some("nonexistent".to_string()),
            "criticality",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sort_by_criticality() {
        let (_temp_dir, store) = setup_test_store().await;
        let entries = store.list().await.unwrap();

        // Verify test data has different criticality scores
        assert_eq!(entries.len(), 3);
        let criticalities: Vec<f64> = entries.iter().map(|e| e.criticality).collect();
        assert!(criticalities.contains(&0.9));
        assert!(criticalities.contains(&0.7));
        assert!(criticalities.contains(&0.5));
    }

    #[tokio::test]
    async fn test_sort_by_created() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            None,
            "created",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sort_by_updated() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            None,
            "updated",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sort_by_type() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            None,
            "type",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_sort_field() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            None,
            "invalid",
            false,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid sort field"));
    }

    #[tokio::test]
    async fn test_reverse_sort() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            None,
            "criticality",
            true,
            None,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_limit() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            None,
            "criticality",
            false,
            Some(2),
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_combined_filters_and_sorting() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);
        let registry = InMemoryRegistry::new();

        let result = run_list(
            temp_dir.path(),
            &registry,
            vec![],
            vec![],
            None,
            Some("app.core".to_string()),
            "criticality",
            false,
            Some(1),
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }
}
