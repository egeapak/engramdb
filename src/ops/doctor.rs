//! Store health check operation.

use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;
use tokio::fs as async_fs;

/// Result of a doctor/health check operation.
pub struct DoctorResult {
    /// Total memories in the index.
    pub indexed: usize,
    /// Total memory files on disk.
    pub on_disk: usize,
    /// IDs in index but missing from disk (stale index entries).
    pub stale_entries: Vec<String>,
    /// Files on disk but missing from index (orphaned files).
    pub orphaned_files: Vec<String>,
    /// Whether the store is healthy (no issues found).
    pub healthy: bool,
}

/// Run a health check on the memory store.
///
/// Compares the LanceDB index against actual memory files on disk to detect:
/// - Stale index entries: IDs in LanceDB with no backing `.md` file
/// - Orphaned files: `.md` files not tracked in the LanceDB index
pub async fn doctor(store: &MemoryStore) -> Result<DoctorResult> {
    let entries = store.list().await?;
    let indexed = entries.len();

    let mut stale_entries = Vec::new();
    for entry in &entries {
        if store.get(&entry.id).await.is_err() {
            stale_entries.push(entry.id.clone());
        }
    }

    // Scan disk for .md files not in the index
    let indexed_ids: std::collections::HashSet<&str> =
        entries.iter().map(|e| e.id.as_str()).collect();

    let mut on_disk = 0;
    let mut orphaned_files = Vec::new();

    let memories_dir = store.project_dir.join(".engramdb").join("memories");
    collect_orphans(
        &memories_dir,
        &indexed_ids,
        &mut on_disk,
        &mut orphaned_files,
    )
    .await;

    // Also check personal memories directory
    if let Ok(personal_dir) = crate::storage::paths::personal_memories_dir(&store.project_id) {
        collect_orphans(
            &personal_dir,
            &indexed_ids,
            &mut on_disk,
            &mut orphaned_files,
        )
        .await;
    }

    let healthy = stale_entries.is_empty() && orphaned_files.is_empty();

    Ok(DoctorResult {
        indexed,
        on_disk,
        stale_entries,
        orphaned_files,
        healthy,
    })
}

/// Scan a directory for `.md` memory files, counting total and collecting orphans.
async fn collect_orphans(
    dir: &Path,
    indexed_ids: &std::collections::HashSet<&str>,
    on_disk: &mut usize,
    orphaned: &mut Vec<String>,
) {
    if !dir.exists() {
        return;
    }
    let mut read_dir = match async_fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "md") {
            *on_disk += 1;
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if !indexed_ids.contains(stem) {
                    orphaned.push(stem.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_doctor_healthy_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        let result = doctor(&store).await.unwrap();
        assert!(result.healthy);
        assert_eq!(result.indexed, 1);
        assert_eq!(result.on_disk, 1);
        assert!(result.stale_entries.is_empty());
        assert!(result.orphaned_files.is_empty());
    }

    #[tokio::test]
    async fn test_doctor_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let _store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let store = MemoryStore::open(temp_dir.path(), &registry).await.unwrap();
        let result = doctor(&store).await.unwrap();
        assert!(result.healthy);
        assert_eq!(result.indexed, 0);
        assert_eq!(result.on_disk, 0);
    }

    #[tokio::test]
    async fn test_doctor_detects_orphaned_file() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Write an orphaned .md file directly to disk
        let orphan_path = temp_dir
            .path()
            .join(".engramdb")
            .join("memories")
            .join("orphan-id-001.md");
        async_fs::write(&orphan_path, "---\nid: orphan-id-001\n---\n")
            .await
            .unwrap();

        let result = doctor(&store).await.unwrap();
        assert!(!result.healthy);
        assert_eq!(result.on_disk, 1);
        assert_eq!(result.orphaned_files, vec!["orphan-id-001"]);
        assert!(result.stale_entries.is_empty());
    }

    #[tokio::test]
    async fn test_doctor_detects_stale_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Create a memory normally, then delete the file behind the store's back
        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        let id = store.create(&mem).await.unwrap();

        let file_path = temp_dir
            .path()
            .join(".engramdb")
            .join("memories")
            .join(format!("{}.md", id));
        async_fs::remove_file(&file_path).await.unwrap();

        let result = doctor(&store).await.unwrap();
        assert!(!result.healthy);
        assert_eq!(result.indexed, 1);
        assert_eq!(result.on_disk, 0);
        assert_eq!(result.stale_entries.len(), 1);
        assert_eq!(result.stale_entries[0], id);
    }
}
