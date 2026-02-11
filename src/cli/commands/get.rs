//! Get a single memory by ID.

use crate::cli::output::OutputFormatter;
use crate::ops::get_memory;
use crate::storage::{paths, MemoryStore, RegistryBackend};
use crate::types::Visibility;
use anyhow::Result;
use std::path::Path;
use tokio::fs;

/// Retrieve and display a single memory by ID.
///
/// Supports prefix matching for the memory ID.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `id` - The memory ID or prefix
/// * `full` - Show complete details without truncation
/// * `raw` - Output the raw markdown file contents
/// * `path_only` - Print the memory's file path instead of content
/// * `formatter` - Output formatter for displaying the memory
pub async fn run_get(
    dir: &Path,
    registry: &dyn RegistryBackend,
    id: &str,
    full: bool,
    raw: bool,
    path_only: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let memory = get_memory(&store, id).await?;

    // Handle --path flag: print file path and exit
    if path_only {
        let file_path = match memory.visibility {
            Visibility::Shared => paths::memories_dir(dir).join(format!("{}.md", memory.id)),
            Visibility::Personal => {
                paths::personal_memories_dir(&store.project_id)?.join(format!("{}.md", memory.id))
            }
        };
        println!("{}", file_path.display());
        return Ok(());
    }

    // Handle --raw flag: read and print raw markdown file
    if raw {
        let file_path = match memory.visibility {
            Visibility::Shared => paths::memories_dir(dir).join(format!("{}.md", memory.id)),
            Visibility::Personal => {
                paths::personal_memories_dir(&store.project_id)?.join(format!("{}.md", memory.id))
            }
        };
        let content = fs::read_to_string(&file_path).await?;
        print!("{}", content);
        return Ok(());
    }

    // Handle --full flag: show complete details without truncation
    if full {
        formatter.print_memory_full(&memory);
    } else {
        formatter.print_memory(&memory);
    }

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
    async fn test_get_existing_memory() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_get(
            temp_dir.path(),
            &registry,
            "mem-test-001-aaa",
            false,
            false,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_prefix_match() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        // Use a unique prefix that matches only one memory
        let result = run_get(
            temp_dir.path(),
            &registry,
            "mem-test-001",
            false,
            false,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let (temp_dir, _store, registry) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_get(
            temp_dir.path(),
            &registry,
            "nonexistent-id",
            false,
            false,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_err());
    }
}
