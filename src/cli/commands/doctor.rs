//! Doctor (health check) command.

use crate::cli::output::OutputFormatter;
use crate::ops::doctor;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Run a store health check.
pub async fn run_doctor(
    dir: &Path,
    registry: &dyn RegistryBackend,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    let result = doctor(&store).await?;

    if result.healthy {
        formatter.print_success(&format!(
            "Store is healthy. {} memories indexed, {} on disk.",
            result.indexed, result.on_disk
        ));
    } else {
        if !result.stale_entries.is_empty() {
            formatter.print_warning(&format!(
                "{} stale index entries (in index but missing from disk):",
                result.stale_entries.len()
            ));
            for id in &result.stale_entries {
                println!("  {}", &id[..13.min(id.len())]);
            }
        }
        if !result.orphaned_files.is_empty() {
            formatter.print_warning(&format!(
                "{} orphaned files (on disk but not in index):",
                result.orphaned_files.len()
            ));
            for id in &result.orphaned_files {
                println!("  {}", &id[..13.min(id.len())]);
            }
        }
        formatter.print_message("\nRun `engramdb reindex` to repair.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::output::OutputFormatter;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_doctor_healthy() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(temp_dir.path(), &registry, &formatter).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_doctor_with_orphan() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Write orphaned file
        let orphan_path = temp_dir
            .path()
            .join(".engramdb")
            .join("memories")
            .join("orphan-001.md");
        tokio::fs::write(&orphan_path, "---\nid: orphan-001\n---\n")
            .await
            .unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(temp_dir.path(), &registry, &formatter).await;
        assert!(result.is_ok());
    }
}
