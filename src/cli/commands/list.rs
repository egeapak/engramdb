//! List all memories with optional filtering.

use crate::cli::output::OutputFormatter;
use crate::ops::{self, ListParams};
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// List all memories, optionally filtered by type, tags, status, and scope.
///
/// Returns index entries (lightweight summaries) rather than full memory data.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
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
    global: bool,
    type_filter: Vec<String>,
    tags_filter: Vec<String>,
    status_filter: Option<String>,
    scope_filter: Option<String>,
    sort_field: &str,
    reverse: bool,
    limit: Option<usize>,
    verbose: bool,
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

    let parsed_sort = ops::parse_sort_field(sort_field)?;

    let params = ListParams {
        types: if type_filter.is_empty() {
            None
        } else {
            Some(type_filter)
        },
        tags: if tags_filter.is_empty() {
            None
        } else {
            Some(tags_filter)
        },
        status: status_filter,
        scope: scope_filter,
        sort_field: parsed_sort,
        reverse,
        limit,
    };

    let entries = ops::list_memories(&store, &params).await?;
    formatter.print_memory_list(&entries, verbose);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::list::list_memories;
    use crate::ops::SortField;
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
            title: None,
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

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            Some("src/main.rs".to_string()),
            "criticality",
            false,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_scope_filter_logical() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            Some("app.core".to_string()),
            "criticality",
            false,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_scope_filter_no_match() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            Some("nonexistent".to_string()),
            "criticality",
            false,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sort_by_criticality() {
        let (_temp_dir, store) = setup_test_store().await;
        let entries = store.list_summary().await.unwrap();

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

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            None,
            "created",
            false,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sort_by_updated() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            None,
            "updated",
            false,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sort_by_type() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            None,
            "type",
            false,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_sort_field() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            None,
            "invalid",
            false,
            None,
            false,
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

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            None,
            "criticality",
            true,
            None,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_limit() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            None,
            "criticality",
            false,
            Some(2),
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_combined_filters_and_sorting() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_list(
            temp_dir.path(),
            false,
            vec![],
            vec![],
            None,
            Some("app.core".to_string()),
            "criticality",
            false,
            Some(1),
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_ops_list_memories_directly() {
        let (_temp_dir, store) = setup_test_store().await;

        let params = ListParams {
            types: None,
            tags: None,
            status: None,
            scope: Some("app.core".to_string()),
            sort_field: SortField::Criticality,
            reverse: false,
            limit: None,
        };

        let entries = list_memories(&store, &params).await.unwrap();
        assert_eq!(entries.len(), 2); // mem1 and mem3 have app.core
    }

    #[tokio::test]
    async fn test_ops_list_memories_with_limit() {
        let (_temp_dir, store) = setup_test_store().await;

        let params = ListParams {
            types: None,
            tags: None,
            status: None,
            scope: None,
            sort_field: SortField::Criticality,
            reverse: false,
            limit: Some(1),
        };

        let entries = list_memories(&store, &params).await.unwrap();
        assert_eq!(entries.len(), 1);
    }
}
