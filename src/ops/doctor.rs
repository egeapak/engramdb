//! Store health check operation.

use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;
use tokio::fs as async_fs;

/// Result of a doctor/health check operation.
#[must_use]
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
    let ids = store.list_ids().await?;
    let indexed = ids.len();

    let existing = store
        .batch_exists(&ids)
        .await
        .map_err(|e| anyhow::anyhow!("batch existence check failed: {}", e))?;
    let stale_entries: Vec<String> = ids
        .iter()
        .filter(|id| !existing.contains(id.as_str()))
        .cloned()
        .collect();

    // Scan disk for .md files not in the index
    let indexed_ids: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();

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
        Err(e) => {
            tracing::warn!("Failed to read directory {}: {}", dir.display(), e);
            return;
        }
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
#[must_use]
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
    checks.push(check_binary_on_path().await);

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
async fn check_binary_on_path() -> EnvironmentCheck {
    match tokio::process::Command::new("engramdb")
        .arg("--version")
        .output()
        .await
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
    let has_models = if cache_dir.exists() {
        match async_fs::read_dir(&cache_dir).await {
            Ok(mut entries) => entries.next_entry().await.ok().flatten().is_some(),
            Err(_) => false,
        }
    } else {
        false
    };

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

    // --- Group 1: doctor() store health ---

    #[tokio::test]
    async fn test_doctor_multiple_stale_entries() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut ids = Vec::new();
        for i in 0..3 {
            let summary = format!("Stale {}", i);
            let content = format!("Content {}", i);
            let mem = Memory::new(
                MemoryType::Decision,
                &summary,
                &content,
                Provenance::human(),
            );
            ids.push(store.create(&mem).await.unwrap());
        }

        // Delete all files behind the store's back
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        for id in &ids {
            async_fs::remove_file(memories_dir.join(format!("{}.md", id)))
                .await
                .unwrap();
        }

        let result = doctor(&store).await.unwrap();
        assert!(!result.healthy);
        assert_eq!(result.indexed, 3);
        assert_eq!(result.on_disk, 0);
        assert_eq!(result.stale_entries.len(), 3);
        for id in &ids {
            assert!(result.stale_entries.contains(id));
        }
    }

    #[tokio::test]
    async fn test_doctor_multiple_orphaned_files() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        let orphan_names = ["orphan-aaa", "orphan-bbb", "orphan-ccc"];
        for name in &orphan_names {
            async_fs::write(
                memories_dir.join(format!("{}.md", name)),
                format!("---\nid: {}\n---\n", name),
            )
            .await
            .unwrap();
        }

        let result = doctor(&store).await.unwrap();
        assert!(!result.healthy);
        assert_eq!(result.on_disk, 3);
        assert_eq!(result.orphaned_files.len(), 3);
        for name in &orphan_names {
            assert!(
                result.orphaned_files.contains(&name.to_string()),
                "missing orphan: {}",
                name
            );
        }
        assert!(result.stale_entries.is_empty());
    }

    #[tokio::test]
    async fn test_doctor_mixed_stale_and_orphaned() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Create a memory then delete its file (stale)
        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        let stale_id = store.create(&mem).await.unwrap();
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        async_fs::remove_file(memories_dir.join(format!("{}.md", stale_id)))
            .await
            .unwrap();

        // Write an orphaned file (not in index)
        async_fs::write(
            memories_dir.join("orphan-mixed.md"),
            "---\nid: orphan-mixed\n---\n",
        )
        .await
        .unwrap();

        let result = doctor(&store).await.unwrap();
        assert!(!result.healthy);
        assert_eq!(result.stale_entries.len(), 1);
        assert_eq!(result.stale_entries[0], stale_id);
        assert_eq!(result.orphaned_files.len(), 1);
        assert_eq!(result.orphaned_files[0], "orphan-mixed");
    }

    #[tokio::test]
    async fn test_doctor_non_md_files_ignored() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        // Write non-.md files — these should NOT be counted
        async_fs::write(memories_dir.join("notes.txt"), "text file")
            .await
            .unwrap();
        async_fs::write(memories_dir.join("data.json"), "{}")
            .await
            .unwrap();
        async_fs::write(memories_dir.join("noextension"), "bare file")
            .await
            .unwrap();

        // Also write one .md orphan to prove only .md is counted
        async_fs::write(
            memories_dir.join("real-orphan.md"),
            "---\nid: real-orphan\n---\n",
        )
        .await
        .unwrap();

        let result = doctor(&store).await.unwrap();
        assert!(!result.healthy);
        assert_eq!(result.on_disk, 1); // only the .md file
        assert_eq!(result.orphaned_files, vec!["real-orphan"]);
    }

    #[tokio::test]
    async fn test_doctor_many_memories_healthy() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let count = 50;
        for i in 0..count {
            let summary = format!("Summary {}", i);
            let content = format!("Content {}", i);
            let mem = Memory::new(MemoryType::Context, &summary, &content, Provenance::human());
            store.create(&mem).await.unwrap();
        }

        let result = doctor(&store).await.unwrap();
        assert!(result.healthy);
        assert_eq!(result.indexed, count);
        assert_eq!(result.on_disk, count);
        assert!(result.stale_entries.is_empty());
        assert!(result.orphaned_files.is_empty());
    }

    #[tokio::test]
    async fn test_doctor_personal_memories_orphan() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Write orphaned .md file in the personal memories dir
        if let Ok(personal_dir) = crate::storage::paths::personal_memories_dir(&store.project_id) {
            async_fs::create_dir_all(&personal_dir).await.unwrap();
            async_fs::write(
                personal_dir.join("personal-orphan.md"),
                "---\nid: personal-orphan\n---\n",
            )
            .await
            .unwrap();

            let result = doctor(&store).await.unwrap();
            assert!(!result.healthy);
            assert!(result
                .orphaned_files
                .contains(&"personal-orphan".to_string()));
            // on_disk should include the personal orphan
            assert!(result.on_disk >= 1);
        }
    }

    #[tokio::test]
    async fn test_doctor_empty_memories_dir() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Store is initialized but no memories created
        let result = doctor(&store).await.unwrap();
        assert!(result.healthy);
        assert_eq!(result.indexed, 0);
        assert_eq!(result.on_disk, 0);
        assert!(result.stale_entries.is_empty());
        assert!(result.orphaned_files.is_empty());
    }

    // --- Group 3: doctor_environment() ---

    #[tokio::test]
    async fn test_environment_returns_all_check_names() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let names: Vec<&str> = result.checks.iter().map(|c| c.name.as_str()).collect();

        assert!(names.contains(&"Binary on PATH"), "missing Binary on PATH");
        assert!(
            names.contains(&"Claude Code plugin"),
            "missing Claude Code plugin"
        );
        assert!(
            names.contains(&"Store initialized"),
            "missing Store initialized"
        );
        assert!(
            names.contains(&"Embedding model"),
            "missing Embedding model"
        );
        assert!(names.contains(&"Store health"), "missing Store health");
    }

    #[tokio::test]
    async fn test_environment_store_not_initialized() {
        let temp_dir = TempDir::new().unwrap();
        // No init — .engramdb/ does not exist

        let result = doctor_environment(temp_dir.path(), None).await;
        let store_check = result
            .checks
            .iter()
            .find(|c| c.name == "Store initialized")
            .unwrap();
        assert!(!store_check.passed);
        assert_eq!(store_check.message, "not found");
        assert!(store_check.suggestion.as_ref().unwrap().contains("init"));
    }

    #[tokio::test]
    async fn test_environment_store_initialized() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), None).await;
        let store_check = result
            .checks
            .iter()
            .find(|c| c.name == "Store initialized")
            .unwrap();
        assert!(store_check.passed);
        assert!(store_check.message.contains(".engramdb/"));
        assert!(store_check.suggestion.is_none());
    }

    #[tokio::test]
    async fn test_environment_healthy_store_check() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let health_check = result
            .checks
            .iter()
            .find(|c| c.name == "Store health")
            .unwrap();
        assert!(health_check.passed);
        assert!(health_check.message.contains("1 memories indexed"));
        assert!(health_check.message.contains("1 on disk"));
        assert!(health_check.suggestion.is_none());

        // store_check should also be populated
        let sc = result.store_check.as_ref().unwrap();
        assert!(sc.healthy);
        assert_eq!(sc.indexed, 1);
    }

    #[tokio::test]
    async fn test_environment_unhealthy_store_check() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Write an orphan to make the store unhealthy
        let memories_dir = temp_dir.path().join(".engramdb").join("memories");
        async_fs::write(
            memories_dir.join("orphan-env.md"),
            "---\nid: orphan-env\n---\n",
        )
        .await
        .unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let health_check = result
            .checks
            .iter()
            .find(|c| c.name == "Store health")
            .unwrap();
        assert!(!health_check.passed);
        assert!(health_check
            .suggestion
            .as_ref()
            .unwrap()
            .contains("reindex"));
    }

    #[tokio::test]
    async fn test_environment_no_store_skips_health() {
        let temp_dir = TempDir::new().unwrap();

        let result = doctor_environment(temp_dir.path(), None).await;
        let health_check = result.checks.iter().find(|c| c.name == "Store health");
        assert!(
            health_check.is_none(),
            "Store health should not appear when no store is given"
        );
        assert!(result.store_check.is_none());
    }

    #[tokio::test]
    async fn test_environment_all_passed_reflects_checks() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let expected = result.checks.iter().all(|c| c.passed);
        assert_eq!(result.all_passed, expected);
    }

    #[tokio::test]
    async fn test_environment_all_passed_false_on_failure() {
        let temp_dir = TempDir::new().unwrap();
        // No init — "Store initialized" will fail

        let result = doctor_environment(temp_dir.path(), None).await;
        assert!(!result.all_passed);
    }

    #[tokio::test]
    async fn test_environment_serializes_to_json() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let json = serde_json::to_string(&result).unwrap();

        assert!(json.contains("\"checks\""));
        assert!(json.contains("\"all_passed\""));
        assert!(json.contains("\"store_check\""));
        assert!(json.contains("\"Store initialized\""));
    }

    #[tokio::test]
    async fn test_check_binary_on_path_returns_result() {
        // check_binary_on_path is async and should always return an EnvironmentCheck
        // (whether or not the binary is installed)
        let result = check_binary_on_path().await;
        assert_eq!(result.name, "Binary on PATH");
        // We can't guarantee the binary is installed in CI, but it should not panic
        assert!(!result.message.is_empty());
    }

    #[tokio::test]
    async fn test_check_embedding_model_cached_missing_dir() {
        // Point at a dir with no .engramdb/config.toml — should gracefully return false
        let temp_dir = TempDir::new().unwrap();
        let result = check_embedding_model_cached(temp_dir.path()).await;
        assert_eq!(result.name, "Embedding model");
        assert!(!result.passed);
        assert_eq!(result.message, "not cached");
    }
}
