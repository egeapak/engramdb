//! Garbage collection command.

use crate::cli::output::{short_id, OutputFormatter};
use crate::cli::validation::validate_score;
use crate::ops::gc_memories;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// Run garbage collection.
///
/// Default mode is dry-run (shows what would be deleted).
/// Use --confirm to actually delete.
pub async fn run_gc(
    dir: &Path,
    global: bool,
    confirm: bool,
    threshold: Option<f64>,
    formatter: &OutputFormatter,
) -> Result<()> {
    if let Some(t) = threshold {
        validate_score(t, "threshold")?;
    }

    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path).await?;

    let dry_run = !confirm;
    let result = gc_memories(&store, &config, dry_run, threshold).await?;

    if !result.stale_entries.is_empty() {
        formatter.print_warning(&format!(
            "Found {} stale index entries (missing data). Run `engramdb reindex` to fix.",
            result.stale_entries.len()
        ));
    }

    if result.count == 0 {
        formatter.print_message("No memories eligible for garbage collection.");
    } else if dry_run {
        formatter.print_message(&format!(
            "Found {} memories eligible for removal (dry run):",
            result.count
        ));
        for id in &result.removed {
            let id_short = short_id(id);
            match store.get(id).await {
                Ok(memory) => {
                    println!(
                        "  {} {:8}  {} (criticality: {:.2})",
                        id_short,
                        format!("{:?}", memory.type_),
                        memory.summary,
                        memory.criticality
                    );
                }
                Err(_) => {
                    println!("  {}", id_short);
                }
            }
        }
        formatter.print_message("\nRun with --confirm to delete these memories.");
    } else {
        formatter.print_success(&format!("Removed {} memories.", result.count));
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
    async fn test_gc_dry_run_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_gc_dry_run_with_memories() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;
        assert!(result.is_ok());
    }
}
