//! Get a single memory by ID.

use crate::cli::output::OutputFormatter;
use crate::ops::get_memory;
use crate::storage::{memory_file, paths, MemoryStore};
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
/// * `id` - The memory ID or prefix
/// * `full` - Show complete details without truncation
/// * `raw` - Output the raw markdown file contents
/// * `path_only` - Print the memory's file path instead of content
/// * `formatter` - Output formatter for displaying the memory
pub async fn run_get(
    dir: &Path,
    global: bool,
    id: &str,
    full: bool,
    raw: bool,
    path_only: bool,
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
    let memory = get_memory(&store, id).await?;

    // Handle --path flag: print file path and exit
    if path_only {
        let filename = memory_file::memory_filename(&memory);
        let file_path = match memory.visibility {
            Visibility::Shared => paths::memories_dir(dir).join(&filename),
            Visibility::Personal => {
                paths::personal_memories_dir(&store.project_id)?.join(&filename)
            }
        };
        println!("{}", file_path.display());
        return Ok(());
    }

    // Handle --raw flag: read and print raw markdown file
    if raw {
        let filename = memory_file::memory_filename(&memory);
        let file_path = match memory.visibility {
            Visibility::Shared => paths::memories_dir(dir).join(&filename),
            Visibility::Personal => {
                paths::personal_memories_dir(&store.project_id)?.join(&filename)
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

    #[tokio::test]
    async fn test_get_existing_memory() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_get(
            temp_dir.path(),
            false,
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
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        // Use a unique prefix that matches only one memory
        let result = run_get(
            temp_dir.path(),
            false,
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
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_get(
            temp_dir.path(),
            false,
            "nonexistent-id",
            false,
            false,
            false,
            &formatter,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_global_targets_global_store() {
        // Seed a memory directly into the global store (no embeddings).
        let _lock = crate::storage::test_support::acquire_global_test_lock().await;
        let global = MemoryStore::open_global().await.unwrap();
        let mem = create_test_memory("global-mem-001-zzz", MemoryType::Decision, 0.9);
        global.create(&mem).await.unwrap();

        // A project dir that was never initialized as an EngramDB store.
        let temp_dir = TempDir::new().unwrap();
        let formatter = OutputFormatter::new(None, false, true);

        // With --global, the memory resolves out of the global store...
        let global_result = run_get(
            temp_dir.path(),
            true,
            "global-mem-001-zzz",
            false,
            false,
            false,
            &formatter,
        )
        .await;
        assert!(global_result.is_ok(), "global lookup should succeed");

        // ...while the same command without --global targets the (uninitialized)
        // project store and fails — proving the flag changes the target store.
        let project_result = run_get(
            temp_dir.path(),
            false,
            "global-mem-001-zzz",
            false,
            false,
            false,
            &formatter,
        )
        .await;
        assert!(
            project_result.is_err(),
            "project lookup must not see global memories"
        );
    }
}
