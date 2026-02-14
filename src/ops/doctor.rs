//! Store health check operation.

use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;
use tokio::fs as async_fs;

/// Result of a doctor/health check operation.
#[derive(Debug, serde::Serialize)]
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

/// A single environment check result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvironmentCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
    pub suggestion: Option<String>,
}

/// Full environment doctor result including all checks.
#[derive(Debug, serde::Serialize)]
pub struct EnvironmentDoctorResult {
    pub checks: Vec<EnvironmentCheck>,
    pub all_passed: bool,
    pub store_check: Option<DoctorResult>,
}

/// Run a full environment diagnostic.
///
/// Checks binary availability, Claude Code plugin installation, store initialization,
/// embedding model cache, and (if a store exists) store health.
pub async fn doctor_environment(
    dir: &Path,
    store: Option<&MemoryStore>,
) -> EnvironmentDoctorResult {
    let mut checks = Vec::new();

    // 1. Binary on PATH
    checks.push(check_binary_on_path());

    // 2. Claude Code plugin installed
    checks.push(check_claude_plugin());

    // 3. Store initialized
    let store_initialized = dir.join(".engramdb").exists();
    checks.push(EnvironmentCheck {
        name: "Store initialized".to_string(),
        passed: store_initialized,
        message: if store_initialized {
            ".engramdb/ exists".to_string()
        } else {
            "not found".to_string()
        },
        suggestion: if store_initialized {
            None
        } else {
            Some("Run `engramdb init` to initialize a store".to_string())
        },
    });

    // 4. Embedding model cached
    checks.push(check_embedding_model_cached(dir).await);

    // 5. Store health (only if store is available)
    let store_check = if let Some(s) = store {
        match doctor(s).await {
            Ok(result) => {
                checks.push(EnvironmentCheck {
                    name: "Store health".to_string(),
                    passed: result.healthy,
                    message: format!(
                        "{} memories indexed, {} on disk",
                        result.indexed, result.on_disk
                    ),
                    suggestion: if result.healthy {
                        None
                    } else {
                        Some("Run `engramdb reindex` to repair".to_string())
                    },
                });
                Some(result)
            }
            Err(e) => {
                checks.push(EnvironmentCheck {
                    name: "Store health".to_string(),
                    passed: false,
                    message: format!("check failed: {}", e),
                    suggestion: Some("Run `engramdb reindex` to repair".to_string()),
                });
                None
            }
        }
    } else {
        None
    };

    let all_passed = checks.iter().all(|c| c.passed);

    EnvironmentDoctorResult {
        checks,
        all_passed,
        store_check,
    }
}

/// Check if `engramdb` binary is on PATH.
fn check_binary_on_path() -> EnvironmentCheck {
    match std::process::Command::new("engramdb")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            EnvironmentCheck {
                name: "Binary on PATH".to_string(),
                passed: true,
                message: version,
                suggestion: None,
            }
        }
        _ => EnvironmentCheck {
            name: "Binary on PATH".to_string(),
            passed: false,
            message: "not found".to_string(),
            suggestion: Some("Install with `brew install engramdb`".to_string()),
        },
    }
}

/// Check if the Claude Code plugin is installed.
fn check_claude_plugin() -> EnvironmentCheck {
    let plugin_file = dirs::home_dir().map(|h| {
        h.join(".claude")
            .join("plugins")
            .join("installed_plugins.json")
    });

    let found = plugin_file
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|contents| contents.contains("engramdb"))
        .unwrap_or(false);

    EnvironmentCheck {
        name: "Claude Code plugin".to_string(),
        passed: found,
        message: if found {
            "installed".to_string()
        } else {
            "not found".to_string()
        },
        suggestion: if found {
            None
        } else {
            Some("Install with `claude plugin add https://github.com/egeapak/engramdb`".to_string())
        },
    }
}

/// Check if the embedding model is cached on disk.
async fn check_embedding_model_cached(dir: &Path) -> EnvironmentCheck {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
        .join("engramdb")
        .join("models");

    // Try to read the configured model name from the project config
    let model_name = if let Ok(config) =
        crate::storage::config::load_config(&dir.join(".engramdb").join("config.toml")).await
    {
        config.embeddings.provider
    } else {
        "all-MiniLM-L6-v2".to_string()
    };

    // Check if the cache dir exists and has at least one subdirectory (model files)
    let has_models = cache_dir.exists()
        && std::fs::read_dir(&cache_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).count() > 0)
            .unwrap_or(false);

    EnvironmentCheck {
        name: "Embedding model".to_string(),
        passed: has_models,
        message: if has_models {
            format!("{} cached", model_name)
        } else {
            "not cached".to_string()
        },
        suggestion: if has_models {
            None
        } else {
            Some("Run `engramdb init` to download the embedding model".to_string())
        },
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
