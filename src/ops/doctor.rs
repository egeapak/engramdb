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

/// A group of related environment checks under a section heading.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorSection {
    pub name: String,
    pub checks: Vec<EnvironmentCheck>,
}

/// Full environment doctor result including all checks.
#[must_use]
#[derive(Debug, serde::Serialize)]
pub struct EnvironmentDoctorResult {
    pub sections: Vec<DoctorSection>,
    pub all_passed: bool,
    pub store_check: Option<DoctorResult>,
}

impl EnvironmentDoctorResult {
    /// Flatten all checks from all sections into a single vec (useful for tests).
    pub fn all_checks(&self) -> Vec<&EnvironmentCheck> {
        self.sections.iter().flat_map(|s| &s.checks).collect()
    }
}

/// Run a full environment diagnostic organized into sections.
///
/// Sections: System, Project, Agent, Embeddings, Registry.
pub async fn doctor_environment(
    dir: &Path,
    store: Option<&MemoryStore>,
) -> EnvironmentDoctorResult {
    let mut sections = Vec::new();

    // --- System section ---
    let mut system_checks = Vec::new();
    system_checks.push(check_binary_on_path().await);

    let store_initialized = dir.join(".engramdb").exists();
    system_checks.push(EnvironmentCheck {
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

    let store_check = if let Some(s) = store {
        match doctor(s).await {
            Ok(result) => {
                system_checks.push(EnvironmentCheck {
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
                system_checks.push(EnvironmentCheck {
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
    sections.push(DoctorSection {
        name: "System".to_string(),
        checks: system_checks,
    });

    // --- Load registry once for Project gating + Registry section ---
    let registry_info = load_registry_info(dir).await;

    // --- Project section ---
    if store_initialized && registry_info.in_registry {
        let mut project_checks = Vec::new();
        project_checks.push(check_config_file(dir).await);
        project_checks.push(check_mcp_config(dir));
        sections.push(DoctorSection {
            name: "Project".to_string(),
            checks: project_checks,
        });
    } else {
        sections.push(DoctorSection {
            name: "Project".to_string(),
            checks: vec![EnvironmentCheck {
                name: "Skipped".to_string(),
                passed: true,
                message: "project not initialized or not in registry".to_string(),
                suggestion: Some("Run `engramdb init` to set up this project".to_string()),
            }],
        });
    }

    // --- Agent section ---
    let agent_checks = vec![check_claude_plugin(), check_hook_config()];
    sections.push(DoctorSection {
        name: "Agent".to_string(),
        checks: agent_checks,
    });

    // --- Embeddings section ---
    let mut embeddings_checks = Vec::new();
    embeddings_checks.push(check_embedding_backend(dir).await);
    let cache_dir = crate::storage::paths::model_cache_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".cache/engramdb/models"));
    embeddings_checks.push(check_embedding_model_cached(dir, &cache_dir).await);
    #[cfg(feature = "ollama")]
    embeddings_checks.push(check_ollama_connectivity(dir).await);
    sections.push(DoctorSection {
        name: "Embeddings".to_string(),
        checks: embeddings_checks,
    });

    // --- Registry section ---
    let memory_count = if let Some(s) = store {
        s.list_ids().await.map(|ids| ids.len()).ok()
    } else {
        None
    };
    let registry_checks = build_registry_checks(&registry_info, memory_count);
    sections.push(DoctorSection {
        name: "Registry".to_string(),
        checks: registry_checks,
    });

    let all_passed = sections.iter().flat_map(|s| &s.checks).all(|c| c.passed);

    EnvironmentDoctorResult {
        sections,
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

/// Pre-loaded registry information shared between Project gating and Registry section.
struct RegistryInfo {
    in_registry: bool,
    total_projects: usize,
    reachable_projects: usize,
    loaded: bool,
}

/// Load registry info once for reuse across sections.
async fn load_registry_info(dir: &Path) -> RegistryInfo {
    use crate::storage::{FileRegistry, RegistryBackend};
    let project_id = crate::storage::project_id::compute_project_id(dir);
    let registry = match FileRegistry::global() {
        Ok(r) => r,
        Err(_) => {
            return RegistryInfo {
                in_registry: false,
                total_projects: 0,
                reachable_projects: 0,
                loaded: false,
            };
        }
    };
    match registry.load().await {
        Ok(reg) => {
            let in_registry = reg.projects.iter().any(|e| e.project_id == project_id);
            let total_projects = reg.projects.len();
            let reachable_projects = reg
                .projects
                .iter()
                .filter(|e| {
                    std::path::Path::new(&e.project_path)
                        .join(".engramdb")
                        .exists()
                })
                .count();
            RegistryInfo {
                in_registry,
                total_projects,
                reachable_projects,
                loaded: true,
            }
        }
        Err(_) => RegistryInfo {
            in_registry: false,
            total_projects: 0,
            reachable_projects: 0,
            loaded: false,
        },
    }
}

/// Check `.engramdb/config.toml` syntax and values.
async fn check_config_file(dir: &Path) -> EnvironmentCheck {
    let config_path = dir.join(".engramdb").join("config.toml");
    if !config_path.exists() {
        return EnvironmentCheck {
            name: "Config file".to_string(),
            passed: true,
            message: "not present (using defaults)".to_string(),
            suggestion: None,
        };
    }
    match crate::storage::config::load_config(&config_path).await {
        Ok(config) => match config.validate() {
            Ok(()) => EnvironmentCheck {
                name: "Config file".to_string(),
                passed: true,
                message: ".engramdb/config.toml valid".to_string(),
                suggestion: None,
            },
            Err(e) => EnvironmentCheck {
                name: "Config file".to_string(),
                passed: false,
                message: format!("invalid values: {}", e),
                suggestion: Some("Fix the values in .engramdb/config.toml".to_string()),
            },
        },
        Err(e) => EnvironmentCheck {
            name: "Config file".to_string(),
            passed: false,
            message: format!("parse error: {}", e),
            suggestion: Some("Fix the syntax in .engramdb/config.toml".to_string()),
        },
    }
}

/// Check if project `.mcp.json` exists and references engramdb.
fn check_mcp_config(dir: &Path) -> EnvironmentCheck {
    let mcp_path = dir.join(".mcp.json");
    let found = std::fs::read_to_string(&mcp_path)
        .ok()
        .map(|contents| contents.contains("engramdb"))
        .unwrap_or(false);

    EnvironmentCheck {
        name: "MCP server configuration".to_string(),
        passed: found,
        message: if found {
            "configured".to_string()
        } else {
            "not configured".to_string()
        },
        suggestion: if found {
            None
        } else {
            Some("Add engramdb to .mcp.json, or install the Claude Code plugin".to_string())
        },
    }
}

/// Check `~/.claude/settings.json` for engramdb hook configuration.
fn check_hook_config() -> EnvironmentCheck {
    let found = dirs::home_dir()
        .map(|h| h.join(".claude").join("settings.json"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|contents| contents.contains("engramdb"))
        .unwrap_or(false);

    EnvironmentCheck {
        name: "Hook configuration".to_string(),
        passed: found,
        message: if found {
            "configured".to_string()
        } else {
            "not configured".to_string()
        },
        suggestion: if found {
            None
        } else {
            Some("Install the Claude Code plugin to configure hooks automatically".to_string())
        },
    }
}

/// Informational check showing the active embedding backend and model from config.
async fn check_embedding_backend(dir: &Path) -> EnvironmentCheck {
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();

    EnvironmentCheck {
        name: "Embedding backend".to_string(),
        passed: true,
        message: format!(
            "{} (model: {})",
            config.embeddings.backend, config.embeddings.provider
        ),
        suggestion: None,
    }
}

/// Check Ollama connectivity with a 3-second timeout.
#[cfg(feature = "ollama")]
async fn check_ollama_connectivity(dir: &Path) -> EnvironmentCheck {
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();
    let ollama_is_backend = config.embeddings.backend == crate::types::EmbeddingBackend::Ollama;

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return EnvironmentCheck {
                name: "Ollama connectivity".to_string(),
                passed: !ollama_is_backend,
                message: "HTTP client error".to_string(),
                suggestion: Some("Check reqwest/TLS configuration".to_string()),
            };
        }
    };

    match client.get("http://localhost:11434").send().await {
        Ok(resp) if resp.status().is_success() => EnvironmentCheck {
            name: "Ollama connectivity".to_string(),
            passed: true,
            message: "reachable at http://localhost:11434".to_string(),
            suggestion: None,
        },
        _ => {
            if ollama_is_backend {
                EnvironmentCheck {
                    name: "Ollama connectivity".to_string(),
                    passed: false,
                    message: "unreachable".to_string(),
                    suggestion: Some(
                        "Start Ollama with `ollama serve` or check connection".to_string(),
                    ),
                }
            } else {
                EnvironmentCheck {
                    name: "Ollama connectivity".to_string(),
                    passed: true,
                    message: "unreachable (not configured as backend)".to_string(),
                    suggestion: None,
                }
            }
        }
    }
}

/// Build registry checks from pre-loaded registry info.
fn build_registry_checks(
    info: &RegistryInfo,
    memory_count: Option<usize>,
) -> Vec<EnvironmentCheck> {
    let mut checks = Vec::new();

    if !info.loaded {
        checks.push(EnvironmentCheck {
            name: "Registered projects".to_string(),
            passed: true,
            message: "registry unavailable".to_string(),
            suggestion: None,
        });
        checks.push(EnvironmentCheck {
            name: "Current project in registry".to_string(),
            passed: false,
            message: "registry unavailable".to_string(),
            suggestion: Some("Run `engramdb init` to register this project".to_string()),
        });
        return checks;
    }

    let stale = info.total_projects - info.reachable_projects;
    let msg = if stale > 0 {
        format!(
            "{} registered, {} reachable, {} stale",
            info.total_projects, info.reachable_projects, stale
        )
    } else {
        format!(
            "{} registered, {} reachable",
            info.total_projects, info.reachable_projects
        )
    };
    checks.push(EnvironmentCheck {
        name: "Registered projects".to_string(),
        passed: true,
        message: msg,
        suggestion: if stale > 0 {
            Some("Run `engramdb projects prune` to remove stale entries".to_string())
        } else {
            None
        },
    });

    let current_msg = if info.in_registry {
        match memory_count {
            Some(n) => format!("registered ({} memories)", n),
            None => "registered".to_string(),
        }
    } else {
        "not registered".to_string()
    };
    checks.push(EnvironmentCheck {
        name: "Current project in registry".to_string(),
        passed: info.in_registry,
        message: current_msg,
        suggestion: if info.in_registry {
            None
        } else {
            Some("Run `engramdb init` to register this project".to_string())
        },
    });

    checks
}

/// Check if the embedding model is cached on disk.
async fn check_embedding_model_cached(dir: &Path, cache_dir: &Path) -> EnvironmentCheck {
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

        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
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
        let names: Vec<&str> = result
            .all_checks()
            .iter()
            .map(|c| c.name.as_str())
            .collect();

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
            names.contains(&"Embedding backend"),
            "missing Embedding backend"
        );
        assert!(names.contains(&"Store health"), "missing Store health");
    }

    #[tokio::test]
    async fn test_environment_has_expected_sections() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let section_names: Vec<&str> = result.sections.iter().map(|s| s.name.as_str()).collect();

        assert_eq!(
            section_names,
            vec!["System", "Project", "Agent", "Embeddings", "Registry"]
        );
    }

    #[tokio::test]
    async fn test_environment_store_not_initialized() {
        let temp_dir = TempDir::new().unwrap();

        let result = doctor_environment(temp_dir.path(), None).await;
        let store_check = result
            .all_checks()
            .into_iter()
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
            .all_checks()
            .into_iter()
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
            .all_checks()
            .into_iter()
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
            .all_checks()
            .into_iter()
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
        let health_check = result
            .all_checks()
            .into_iter()
            .find(|c| c.name == "Store health");
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
        let expected = result.all_checks().iter().all(|c| c.passed);
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

        assert!(json.contains("\"sections\""));
        assert!(json.contains("\"all_passed\""));
        assert!(json.contains("\"store_check\""));
        assert!(json.contains("\"Store initialized\""));
    }

    #[tokio::test]
    async fn test_check_binary_on_path_returns_result() {
        let result = check_binary_on_path().await;
        assert_eq!(result.name, "Binary on PATH");
        assert!(!result.message.is_empty());
    }

    #[tokio::test]
    async fn test_check_embedding_model_cached_missing_dir() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();
        let result = check_embedding_model_cached(temp_dir.path(), cache_dir.path()).await;
        assert_eq!(result.name, "Embedding model");
        assert!(!result.passed);
        assert_eq!(result.message, "not cached");
        assert!(result.suggestion.is_some());
    }

    // --- Group 4: new check functions ---

    #[tokio::test]
    async fn test_check_config_file_missing() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_config_file(temp_dir.path()).await;
        assert_eq!(result.name, "Config file");
        assert!(result.passed);
        assert!(result.message.contains("defaults"));
    }

    #[tokio::test]
    async fn test_check_config_file_valid() {
        let temp_dir = TempDir::new().unwrap();
        let engramdb_dir = temp_dir.path().join(".engramdb");
        async_fs::create_dir_all(&engramdb_dir).await.unwrap();
        async_fs::write(engramdb_dir.join("config.toml"), "")
            .await
            .unwrap();

        let result = check_config_file(temp_dir.path()).await;
        assert_eq!(result.name, "Config file");
        assert!(result.passed);
        assert!(result.message.contains("valid"));
    }

    #[tokio::test]
    async fn test_check_config_file_invalid_toml() {
        let temp_dir = TempDir::new().unwrap();
        let engramdb_dir = temp_dir.path().join(".engramdb");
        async_fs::create_dir_all(&engramdb_dir).await.unwrap();
        async_fs::write(engramdb_dir.join("config.toml"), "{{{{not toml")
            .await
            .unwrap();

        let result = check_config_file(temp_dir.path()).await;
        assert_eq!(result.name, "Config file");
        assert!(!result.passed);
        assert!(result.message.contains("parse error"));
    }

    #[test]
    fn test_check_mcp_config_missing() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_mcp_config(temp_dir.path());
        assert_eq!(result.name, "MCP server configuration");
        assert!(!result.passed);
        assert_eq!(result.message, "not configured");
    }

    #[test]
    fn test_check_mcp_config_present() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join(".mcp.json"),
            r#"{"mcpServers": {"engramdb": {}}}"#,
        )
        .unwrap();

        let result = check_mcp_config(temp_dir.path());
        assert_eq!(result.name, "MCP server configuration");
        assert!(result.passed);
        assert_eq!(result.message, "configured");
    }

    #[test]
    fn test_check_hook_config_returns_result() {
        let result = check_hook_config();
        assert_eq!(result.name, "Hook configuration");
        // Can't guarantee state in CI, but it should not panic
        assert!(!result.message.is_empty());
    }

    #[tokio::test]
    async fn test_check_embedding_backend_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_embedding_backend(temp_dir.path()).await;
        assert_eq!(result.name, "Embedding backend");
        assert!(result.passed);
        assert!(result.message.contains("auto"));
    }

    #[tokio::test]
    async fn test_build_registry_checks_returns_two_checks() {
        let temp_dir = TempDir::new().unwrap();
        let info = load_registry_info(temp_dir.path()).await;
        let checks = build_registry_checks(&info, None);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "Registered projects");
        assert_eq!(checks[1].name, "Current project in registry");
    }

    #[tokio::test]
    async fn test_build_registry_checks_shows_memory_count() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 1,
            reachable_projects: 1,
            loaded: true,
        };
        let checks = build_registry_checks(&info, Some(42));
        let current = &checks[1];
        assert!(current.passed);
        assert!(current.message.contains("42 memories"));
    }

    #[tokio::test]
    async fn test_build_registry_checks_shows_reachable_count() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 5,
            reachable_projects: 3,
            loaded: true,
        };
        let checks = build_registry_checks(&info, None);
        assert!(checks[0].message.contains("5 registered"));
        assert!(checks[0].message.contains("3 reachable"));
        assert!(checks[0].message.contains("2 stale"));
        assert!(checks[0].suggestion.as_ref().unwrap().contains("prune"));
    }

    #[tokio::test]
    async fn test_build_registry_checks_no_stale_no_hint() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 2,
            reachable_projects: 2,
            loaded: true,
        };
        let checks = build_registry_checks(&info, None);
        assert!(!checks[0].message.contains("stale"));
        assert!(checks[0].suggestion.is_none());
    }

    #[tokio::test]
    async fn test_project_section_skipped_when_not_initialized() {
        let temp_dir = TempDir::new().unwrap();

        let result = doctor_environment(temp_dir.path(), None).await;
        let project_section = result
            .sections
            .iter()
            .find(|s| s.name == "Project")
            .unwrap();
        assert_eq!(project_section.checks.len(), 1);
        assert_eq!(project_section.checks[0].name, "Skipped");
        assert!(project_section.checks[0].passed);
        assert!(project_section.checks[0]
            .message
            .contains("not initialized"));
    }
}
