//! Store health check operation.

use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::{Path, PathBuf};
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

/// Visual status for an environment check.
///
/// Most checks are binary pass/fail. `Info` is used for purely informational
/// items that should render with an indicative icon instead of the usual
/// pass/fail markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Warn,
    Info,
}

/// A single environment check result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvironmentCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Sub-lines rendered below the check with extra indentation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    /// Visual status override. When `None`, the formatter uses `passed` to
    /// decide between pass and fail icons. Set to `Some(CheckStatus::Info)` for
    /// informational items that should not appear as failures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<CheckStatus>,
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

    // Global disk usage (informational)
    match (
        crate::storage::paths::global_data_dir(),
        crate::storage::paths::model_cache_dir(),
    ) {
        (Ok(data_dir), Ok(cache_dir)) => {
            system_checks.push(check_global_disk_usage(&data_dir, &cache_dir).await);
        }
        _ => {
            system_checks.push(EnvironmentCheck {
                name: "Global disk usage".to_string(),
                passed: false,
                message: "could not determine global data/cache directories".to_string(),
                suggestion: Some("Check platform directory configuration".to_string()),
                details: vec![],
                status: None,
            });
        }
    }

    sections.push(DoctorSection {
        name: "System".to_string(),
        checks: system_checks,
    });

    // --- Load registry once for Project gating + Registry section ---
    let registry_info = load_registry_info(dir).await;

    // --- Project section ---
    let store_initialized = dir.join(".engramdb").exists();
    let store_path = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .join(".engramdb");

    let store_check = if store_initialized {
        let project_id = crate::storage::project_id::compute_project_id(dir);
        let mut project_checks = Vec::new();

        project_checks.push(EnvironmentCheck {
            name: "Store initialized".to_string(),
            passed: true,
            message: ".engramdb/ exists".to_string(),
            suggestion: None,
            details: vec![format!("path: {}", store_path.display())],
            status: None,
        });

        let sc = if let Some(s) = store {
            match doctor(s).await {
                Ok(result) => {
                    project_checks.push(EnvironmentCheck {
                        name: "Store health".to_string(),
                        passed: result.healthy,
                        message: if result.healthy {
                            "healthy".to_string()
                        } else {
                            "mismatch detected".to_string()
                        },
                        suggestion: if result.healthy {
                            None
                        } else {
                            Some("Run `engramdb reindex` to repair".to_string())
                        },
                        details: vec![
                            format!("indexed: {}", result.indexed),
                            format!("on disk: {}", result.on_disk),
                        ],
                        status: None,
                    });
                    Some(result)
                }
                Err(e) => {
                    project_checks.push(EnvironmentCheck {
                        name: "Store health".to_string(),
                        passed: false,
                        message: format!("check failed: {}", e),
                        suggestion: Some("Run `engramdb reindex` to repair".to_string()),
                        details: vec![],
                        status: None,
                    });
                    None
                }
            }
        } else {
            None
        };

        if let Some(s) = store {
            project_checks.push(check_manifest_stats(dir, s).await);
        }
        if let Some(s) = store {
            project_checks.push(check_chunk_orphans(s).await);
        }

        if registry_info.in_registry {
            project_checks.push(check_config_file(dir).await);
            project_checks.push(check_mcp_config_deep(dir));
            project_checks.push(check_write_lock(&project_id).await);
            project_checks.push(check_project_disk_usage(dir, &project_id).await);
        }
        sections.push(DoctorSection {
            name: "Project".to_string(),
            checks: project_checks,
        });
        sc
    } else {
        sections.push(DoctorSection {
            name: "Project".to_string(),
            checks: vec![EnvironmentCheck {
                name: "Store initialized".to_string(),
                passed: false,
                message: "not found".to_string(),
                suggestion: Some("Run `engramdb init` to set up this project".to_string()),
                details: vec![],
                status: None,
            }],
        });
        None
    };

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

    // --- Gitignore section ---
    let mut gitignore_checks = vec![check_global_gitignore()];
    if store_initialized && registry_info.in_registry {
        gitignore_checks.push(check_project_gitignore(dir));
    }
    sections.push(DoctorSection {
        name: "Gitignore".to_string(),
        checks: gitignore_checks,
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
                details: vec![],
                status: None,
            }
        }
        _ => EnvironmentCheck {
            name: "Binary on PATH".to_string(),
            passed: false,
            message: "not found".to_string(),
            suggestion: Some("Install with `brew install engramdb`".to_string()),
            details: vec![],
            status: None,
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
        passed: true,
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
        details: vec![],
        status: if found { None } else { Some(CheckStatus::Warn) },
    }
}

/// Pre-loaded registry information shared between Project gating and Registry section.
struct RegistryInfo {
    in_registry: bool,
    total_projects: usize,
    reachable_projects: usize,
    orphan_dirs: usize,
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
                orphan_dirs: 0,
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

            // Count orphan data directories (on disk but not in registry)
            let registered_ids: std::collections::HashSet<&str> =
                reg.projects.iter().map(|e| e.project_id.as_str()).collect();
            let orphan_dirs = crate::storage::paths::global_data_dir()
                .ok()
                .map(|d| d.join("projects"))
                .filter(|d| d.exists())
                .and_then(|d| std::fs::read_dir(d).ok())
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .filter(|e| {
                            !registered_ids.contains(e.file_name().to_string_lossy().as_ref())
                        })
                        .count()
                })
                .unwrap_or(0);

            RegistryInfo {
                in_registry,
                total_projects,
                reachable_projects,
                orphan_dirs,
                loaded: true,
            }
        }
        Err(_) => RegistryInfo {
            in_registry: false,
            total_projects: 0,
            reachable_projects: 0,
            orphan_dirs: 0,
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
            details: vec![],
            status: None,
        };
    }
    match crate::storage::config::load_config(&config_path).await {
        Ok(config) => match config.validate() {
            Ok(()) => EnvironmentCheck {
                name: "Config file".to_string(),
                passed: true,
                message: ".engramdb/config.toml valid".to_string(),
                suggestion: None,
                details: vec![],
                status: None,
            },
            Err(e) => EnvironmentCheck {
                name: "Config file".to_string(),
                passed: false,
                message: format!("invalid values: {}", e),
                suggestion: Some("Fix the values in .engramdb/config.toml".to_string()),
                details: vec![],
                status: None,
            },
        },
        Err(e) => EnvironmentCheck {
            name: "Config file".to_string(),
            passed: false,
            message: format!("parse error: {}", e),
            suggestion: Some("Fix the syntax in .engramdb/config.toml".to_string()),
            details: vec![],
            status: None,
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
        passed: true,
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
        details: vec![],
        status: if found { None } else { Some(CheckStatus::Warn) },
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
        details: vec![],
        status: None,
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
                details: vec![],
                status: None,
            };
        }
    };

    match client.get("http://localhost:11434").send().await {
        Ok(resp) if resp.status().is_success() => EnvironmentCheck {
            name: "Ollama connectivity".to_string(),
            passed: true,
            message: "reachable at http://localhost:11434".to_string(),
            suggestion: None,
            details: vec![],
            status: None,
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
                    details: vec![],
                    status: None,
                }
            } else {
                EnvironmentCheck {
                    name: "Ollama connectivity".to_string(),
                    passed: true,
                    message: "unreachable (not configured as backend)".to_string(),
                    suggestion: None,
                    details: vec![],
                    status: None,
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
            details: vec![],
            status: None,
        });
        checks.push(EnvironmentCheck {
            name: "Current project in registry".to_string(),
            passed: false,
            message: "registry unavailable".to_string(),
            suggestion: Some("Run `engramdb init` to register this project".to_string()),
            details: vec![],
            status: None,
        });
        return checks;
    }

    let stale = info.total_projects - info.reachable_projects;
    let mut details = vec![
        format!("registered: {}", info.total_projects),
        format!("reachable: {}", info.reachable_projects),
    ];
    if stale > 0 {
        details.push(format!("stale: {}", stale));
    }
    if info.orphan_dirs > 0 {
        details.push(format!("orphan data dirs: {}", info.orphan_dirs));
    }
    let needs_prune = stale > 0 || info.orphan_dirs > 0;
    checks.push(EnvironmentCheck {
        name: "Registered projects".to_string(),
        passed: true,
        message: format!("{} registered", info.total_projects),
        suggestion: if needs_prune {
            Some("Run `engramdb projects prune` to clean up".to_string())
        } else {
            None
        },
        details,
        status: if needs_prune {
            Some(CheckStatus::Warn)
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
        details: vec![],
        status: None,
    });

    checks
}

/// Directory statistics from a single jwalk pass.
struct DirStats {
    total_size: u64,
    file_count: usize,
}

/// Compute directory stats (total size and file count) using jwalk.
///
/// Optionally filters by file extension (e.g. `Some("md")`).
/// Returns zeros if the directory doesn't exist or is unreadable.
fn dir_stats(path: &Path, extension: Option<&str>) -> DirStats {
    if !path.exists() {
        return DirStats {
            total_size: 0,
            file_count: 0,
        };
    }
    let mut total_size = 0u64;
    let mut file_count = 0usize;
    for entry in jwalk::WalkDir::new(path)
        .skip_hidden(false)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Some(ext) = extension {
            if entry.path().extension().and_then(|e| e.to_str()) != Some(ext) {
                continue;
            }
        }
        if let Ok(meta) = entry.metadata() {
            total_size += meta.len();
            file_count += 1;
        }
    }
    DirStats {
        total_size,
        file_count,
    }
}

/// Compute total size of a directory. Convenience wrapper around `dir_stats`.
fn dir_size(path: &Path) -> u64 {
    dir_stats(path, None).total_size
}

/// List top-level subdirectories with their sizes.
fn subdir_sizes(path: &Path) -> Vec<(String, u64)> {
    let mut results = Vec::new();
    if !path.exists() {
        return results;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return results;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if entry.path().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            let size = dir_size(&entry.path());
            results.push((name, size));
        }
    }
    results
}

/// Format a byte count as a human-readable string (KB/MB/GB).
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Check manifest stats against actual store count.
async fn check_manifest_stats(dir: &Path, store: &MemoryStore) -> EnvironmentCheck {
    let manifest_path = dir.join(".engramdb").join("manifest.toml");
    let manifest = match crate::storage::manifest::load_manifest(&manifest_path).await {
        Ok(m) => m,
        Err(e) => {
            return EnvironmentCheck {
                name: "Manifest stats".to_string(),
                passed: false,
                message: format!("failed to load manifest: {}", e),
                suggestion: Some("Run `engramdb reindex` to regenerate manifest".to_string()),
                details: vec![],
                status: None,
            };
        }
    };

    let actual_count = match store.count().await {
        Ok(c) => c,
        Err(e) => {
            return EnvironmentCheck {
                name: "Manifest stats".to_string(),
                passed: false,
                message: format!("failed to count memories: {}", e),
                suggestion: Some("Run `engramdb reindex` to repair".to_string()),
                details: vec![],
                status: None,
            };
        }
    };

    let manifest_count = manifest.stats.memory_count;
    if manifest_count == actual_count {
        EnvironmentCheck {
            name: "Manifest stats".to_string(),
            passed: true,
            message: format!("memory_count {} matches index", manifest_count),
            suggestion: None,
            details: vec![],
            status: None,
        }
    } else {
        EnvironmentCheck {
            name: "Manifest stats".to_string(),
            passed: true,
            message: format!(
                "manifest says {} memories, index has {}",
                manifest_count, actual_count
            ),
            suggestion: Some("Run `engramdb reindex` to fix".to_string()),
            details: vec![],
            status: Some(CheckStatus::Warn),
        }
    }
}

/// Check for stale write locks.
async fn check_write_lock(project_id: &str) -> EnvironmentCheck {
    let lock_path = match crate::storage::paths::global_data_dir() {
        Ok(d) => d.join("projects").join(project_id).join("write.lock"),
        Err(_) => {
            return EnvironmentCheck {
                name: "Write lock".to_string(),
                passed: true,
                message: "could not determine data dir".to_string(),
                suggestion: None,
                details: vec![],
                status: None,
            };
        }
    };

    if !lock_path.exists() {
        return EnvironmentCheck {
            name: "Write lock".to_string(),
            passed: true,
            message: "no lock file".to_string(),
            suggestion: None,
            details: vec![],
            status: None,
        };
    }

    // Try to acquire an exclusive lock (non-blocking)
    use fs4::fs_std::FileExt;
    match std::fs::File::options()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(file) => match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = file.unlock();
                EnvironmentCheck {
                    name: "Write lock".to_string(),
                    passed: true,
                    message: "no active writer".to_string(),
                    suggestion: None,
                    details: vec![],
                    status: None,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => EnvironmentCheck {
                name: "Write lock".to_string(),
                passed: true,
                message: "write lock held by active process".to_string(),
                suggestion: Some(
                    "Another process is writing; concurrent access may cause issues".to_string(),
                ),
                details: vec![],
                status: Some(CheckStatus::Warn),
            },
            Err(e) => EnvironmentCheck {
                name: "Write lock".to_string(),
                passed: false,
                message: format!("lock check failed: {}", e),
                suggestion: Some("Remove stale lock file or investigate the error".to_string()),
                details: vec![],
                status: None,
            },
        },
        Err(e) => EnvironmentCheck {
            name: "Write lock".to_string(),
            passed: false,
            message: format!("could not open lock file: {}", e),
            suggestion: Some("Check file permissions on the lock file".to_string()),
            details: vec![],
            status: None,
        },
    }
}

/// Check for orphaned chunks (chunk memory_ids not in the memories table).
async fn check_chunk_orphans(store: &MemoryStore) -> EnvironmentCheck {
    let memory_ids = match store.list_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            return EnvironmentCheck {
                name: "Chunk index integrity".to_string(),
                passed: false,
                message: format!("failed to list memory ids: {}", e),
                suggestion: Some("Run `engramdb reindex` to repair".to_string()),
                details: vec![],
                status: None,
            };
        }
    };

    let chunk_ids = match store.list_chunk_memory_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            return EnvironmentCheck {
                name: "Chunk index integrity".to_string(),
                passed: false,
                message: format!("failed to list chunk memory_ids: {}", e),
                suggestion: Some("Run `engramdb reindex` to repair".to_string()),
                details: vec![],
                status: None,
            };
        }
    };

    let memory_set: std::collections::HashSet<&str> =
        memory_ids.iter().map(|s| s.as_str()).collect();
    let orphans: Vec<&str> = chunk_ids
        .iter()
        .filter(|id| !memory_set.contains(id.as_str()))
        .map(|s| s.as_str())
        .collect();

    if orphans.is_empty() {
        EnvironmentCheck {
            name: "Chunk index integrity".to_string(),
            passed: true,
            message: format!("{} chunk memory_ids, no orphans", chunk_ids.len()),
            suggestion: None,
            details: vec![],
            status: None,
        }
    } else {
        EnvironmentCheck {
            name: "Chunk index integrity".to_string(),
            passed: false,
            message: format!(
                "{} orphaned chunk memory_id(s) not in memories table",
                orphans.len()
            ),
            suggestion: Some("Run `engramdb reindex` to clean up orphaned chunks".to_string()),
            details: vec![],
            status: None,
        }
    }
}

/// Deep validation of `.mcp.json` — parses JSON and checks structure.
fn check_mcp_config_deep(dir: &Path) -> EnvironmentCheck {
    let mcp_path = dir.join(".mcp.json");
    let content = match std::fs::read_to_string(&mcp_path) {
        Ok(c) => c,
        Err(_) => {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: false,
                message: ".mcp.json not found".to_string(),
                suggestion: Some(
                    "Add engramdb to .mcp.json, or install the Claude Code plugin".to_string(),
                ),
                details: vec![],
                status: None,
            };
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: false,
                message: format!("invalid JSON: {}", e),
                suggestion: Some("Fix the JSON syntax in .mcp.json".to_string()),
                details: vec![],
                status: None,
            };
        }
    };

    let servers = match json.get("mcpServers") {
        Some(s) => s,
        None => {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: false,
                message: "missing 'mcpServers' key".to_string(),
                suggestion: Some(
                    "Add an 'mcpServers' object containing an 'engramdb' entry".to_string(),
                ),
                details: vec![],
                status: None,
            };
        }
    };

    let engramdb = match servers.get("engramdb") {
        Some(e) => e,
        None => {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: false,
                message: "missing 'mcpServers.engramdb' key".to_string(),
                suggestion: Some("Add an 'engramdb' entry under 'mcpServers'".to_string()),
                details: vec![],
                status: None,
            };
        }
    };

    // Check command field
    if let Some(cmd) = engramdb.get("command").and_then(|v| v.as_str()) {
        // Resolve the binary path — check if it exists
        let cmd_path = PathBuf::from(cmd);
        let resolved = if cmd_path.is_absolute() {
            cmd_path
        } else {
            dir.join(&cmd_path)
        };
        if !resolved.exists() {
            // It might be on PATH, check with which
            let on_path = std::process::Command::new("which")
                .arg(cmd)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !on_path {
                return EnvironmentCheck {
                    name: "MCP server configuration".to_string(),
                    passed: false,
                    message: format!("command '{}' not found on disk or PATH", cmd),
                    suggestion: Some("Check the 'command' path in .mcp.json".to_string()),
                    details: vec![],
                    status: None,
                };
            }
        }
    } else {
        return EnvironmentCheck {
            name: "MCP server configuration".to_string(),
            passed: false,
            message: "missing or invalid 'command' field".to_string(),
            suggestion: Some(
                "Add a 'command' string to mcpServers.engramdb in .mcp.json".to_string(),
            ),
            details: vec![],
            status: None,
        };
    }

    // Check args field
    if let Some(args) = engramdb.get("args") {
        if !args.is_array() {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: false,
                message: "'args' field is not an array".to_string(),
                suggestion: Some("Set 'args' to an array of strings in .mcp.json".to_string()),
                details: vec![],
                status: None,
            };
        }
    }
    // args is optional, so missing is fine

    EnvironmentCheck {
        name: "MCP server configuration".to_string(),
        passed: true,
        message: "configured and valid".to_string(),
        suggestion: None,
        details: vec![],
        status: None,
    }
}

/// Report global disk usage with per-model cache breakdown.
async fn check_global_disk_usage(data_dir: &Path, cache_dir: &Path) -> EnvironmentCheck {
    let projects_dir = data_dir.join("projects");
    let project_subdirs = subdir_sizes(&projects_dir);
    let project_count = project_subdirs.len();
    let projects_size: u64 = project_subdirs.iter().map(|(_, s)| s).sum();

    // Per-model breakdown in cache
    let model_subdirs = subdir_sizes(cache_dir);
    let cache_size: u64 = model_subdirs.iter().map(|(_, s)| s).sum();
    let total = projects_size + cache_size;

    let mut details = Vec::new();
    details.push(format!(
        "projects: {} ({} registered)",
        format_bytes(projects_size),
        project_count
    ));
    if model_subdirs.is_empty() {
        details.push("models: no cached models".to_string());
    } else {
        details.push(format!("models: {} total", format_bytes(cache_size)));
        for (name, size) in &model_subdirs {
            let display_name = name.strip_prefix("models--").unwrap_or(name);
            details.push(format!("  {}: {}", display_name, format_bytes(*size)));
        }
    }

    EnvironmentCheck {
        name: "Global disk usage".to_string(),
        passed: true,
        message: format_bytes(total),
        suggestion: None,
        details,
        status: None,
    }
}

/// Report project-specific disk usage with memory count.
async fn check_project_disk_usage(dir: &Path, project_id: &str) -> EnvironmentCheck {
    let shared_dir = dir.join(".engramdb").join("memories");
    let Ok(lancedb_dir) = crate::storage::paths::lancedb_dir(project_id) else {
        return EnvironmentCheck {
            name: "Project disk usage".to_string(),
            passed: false,
            message: "could not determine project data directories".to_string(),
            suggestion: Some("Check platform directory configuration".to_string()),
            details: vec![],
            status: None,
        };
    };
    let Ok(personal_dir) = crate::storage::paths::personal_memories_dir(project_id) else {
        return EnvironmentCheck {
            name: "Project disk usage".to_string(),
            passed: false,
            message: "could not determine project data directories".to_string(),
            suggestion: Some("Check platform directory configuration".to_string()),
            details: vec![],
            status: None,
        };
    };

    let shared = dir_stats(&shared_dir, Some("md"));
    let lance_size = dir_size(&lancedb_dir);
    let personal = dir_stats(&personal_dir, Some("md"));
    let total = shared.total_size + lance_size + personal.total_size;

    EnvironmentCheck {
        name: "Project disk usage".to_string(),
        passed: true,
        message: format_bytes(total),
        suggestion: None,
        details: vec![
            format!(
                "shared: {} ({} memories)",
                format_bytes(shared.total_size),
                shared.file_count
            ),
            format!(
                "personal: {} ({} memories)",
                format_bytes(personal.total_size),
                personal.file_count
            ),
            format!("index: {}", format_bytes(lance_size)),
        ],
        status: None,
    }
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
        details: vec![],
        status: None,
    }
}

/// Check if `.engramdb` is in the user's global git excludes file.
///
/// This is a notification (not a warning) — users can opt in by running
/// `git config --global core.excludesFile` and adding `.engramdb` to that file.
fn check_global_gitignore() -> EnvironmentCheck {
    // 1. Determine the global excludes file path
    let excludes_path = std::process::Command::new("git")
        .args(["config", "--global", "core.excludesFile"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if p.is_empty() {
                None
            } else {
                // Expand ~ to home directory
                if p.starts_with("~/") {
                    dirs::home_dir().map(|h| h.join(&p[2..]))
                } else {
                    Some(PathBuf::from(p))
                }
            }
        })
        .or_else(|| {
            // Default location: $XDG_CONFIG_HOME/git/ignore or ~/.config/git/ignore
            dirs::config_dir().map(|c| c.join("git").join("ignore"))
        });

    let Some(excludes_path) = excludes_path else {
        return EnvironmentCheck {
            name: "Global gitignore".to_string(),
            passed: true,
            message: "no global excludes file configured".to_string(),
            suggestion: Some(
                "Run `git config --global core.excludesFile ~/.config/git/ignore` \
                 and add .engramdb to ignore it globally"
                    .to_string(),
            ),
            details: vec![],
            status: Some(CheckStatus::Info),
        };
    };

    let content = std::fs::read_to_string(&excludes_path).unwrap_or_default();
    let has_engramdb = content
        .lines()
        .any(|line| line.trim() == ".engramdb" || line.trim() == ".engramdb/");

    if has_engramdb {
        EnvironmentCheck {
            name: "Global gitignore".to_string(),
            passed: true,
            message: ".engramdb ignored globally".to_string(),
            suggestion: None,
            details: vec![],
            status: None,
        }
    } else {
        EnvironmentCheck {
            name: "Global gitignore".to_string(),
            passed: true,
            message: ".engramdb not in global excludes".to_string(),
            suggestion: Some(format!(
                "Add .engramdb to {} to ignore it in all repositories",
                excludes_path.display()
            )),
            details: vec![],
            status: Some(CheckStatus::Info),
        }
    }
}

/// Check `.gitignore` status for `.engramdb` in the current project.
///
/// Reports whether `.engramdb` is ignored, explicitly allowed via `!.engramdb/`,
/// or not mentioned. Always passes — this is informational, showing the current
/// state with an indicative style.
fn check_project_gitignore(dir: &Path) -> EnvironmentCheck {
    let gitignore_path = dir.join(".gitignore");
    if !gitignore_path.exists() {
        return EnvironmentCheck {
            name: "Project .gitignore".to_string(),
            passed: true,
            message: "no .gitignore file".to_string(),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
        };
    }

    let content = match std::fs::read_to_string(&gitignore_path) {
        Ok(c) => c,
        Err(_) => {
            return EnvironmentCheck {
                name: "Project .gitignore".to_string(),
                passed: true,
                message: "could not read .gitignore".to_string(),
                suggestion: None,
                details: vec![],
                status: Some(CheckStatus::Info),
            };
        }
    };

    let is_ignored = content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == ".engramdb" || trimmed == ".engramdb/" || trimmed == "/.engramdb"
    });
    let is_negated = content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == "!.engramdb" || trimmed == "!.engramdb/" || trimmed == "!/.engramdb"
    });

    if is_negated {
        EnvironmentCheck {
            name: "Project .gitignore".to_string(),
            passed: true,
            message: ".engramdb explicitly included (!.engramdb/)".to_string(),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
        }
    } else if is_ignored {
        EnvironmentCheck {
            name: "Project .gitignore".to_string(),
            passed: true,
            message: ".engramdb ignored by project".to_string(),
            suggestion: Some(
                "Add !.engramdb/ to .gitignore to opt in to sharing memories via git".to_string(),
            ),
            details: vec![],
            status: Some(CheckStatus::Info),
        }
    } else {
        // Not mentioned at all — might be caught by global ignore or not ignored
        EnvironmentCheck {
            name: "Project .gitignore".to_string(),
            passed: true,
            message: ".engramdb not mentioned in .gitignore".to_string(),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
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
            vec![
                "System",
                "Project",
                "Agent",
                "Embeddings",
                "Registry",
                "Gitignore"
            ]
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
        assert_eq!(health_check.message, "healthy");
        let details_str = health_check.details.join(" ");
        assert!(details_str.contains("indexed: 1"));
        assert!(details_str.contains("on disk: 1"));
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
    fn test_check_mcp_config_deep_missing() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_mcp_config_deep(temp_dir.path());
        assert_eq!(result.name, "MCP server configuration");
        assert!(!result.passed);
        assert!(result.message.contains("not found"));
    }

    #[test]
    fn test_check_mcp_config_deep_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".mcp.json"), "not json {{{").unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        assert!(!result.passed);
        assert!(result.message.contains("invalid JSON"));
    }

    #[test]
    fn test_check_mcp_config_deep_missing_servers_key() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".mcp.json"), r#"{"other": {}}"#).unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        assert!(!result.passed);
        assert!(result.message.contains("mcpServers"));
    }

    #[test]
    fn test_check_mcp_config_deep_missing_engramdb_key() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join(".mcp.json"),
            r#"{"mcpServers": {"other": {}}}"#,
        )
        .unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        assert!(!result.passed);
        assert!(result.message.contains("engramdb"));
    }

    #[test]
    fn test_check_mcp_config_deep_valid() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join(".mcp.json"),
            r#"{"mcpServers": {"engramdb": {"command": "engramdb", "args": ["serve", "--stdio"]}}}"#,
        )
        .unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        assert_eq!(result.name, "MCP server configuration");
        // May not pass if engramdb isn't on PATH in CI, but validates structure
        // Just check it got past the JSON validation
        assert!(
            result.passed || result.message.contains("not found"),
            "unexpected: {}",
            result.message
        );
    }

    #[test]
    fn test_check_mcp_config_deep_missing_command() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join(".mcp.json"),
            r#"{"mcpServers": {"engramdb": {"args": ["serve"]}}}"#,
        )
        .unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        assert!(!result.passed);
        assert!(result.message.contains("command"));
    }

    #[test]
    fn test_check_mcp_config_deep_args_not_array() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join(".mcp.json"),
            r#"{"mcpServers": {"engramdb": {"command": "engramdb", "args": "not-array"}}}"#,
        )
        .unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        // engramdb might not be on PATH, so check it either fails on args or on command
        if result.passed {
            panic!("should fail with non-array args");
        }
        assert!(
            result.message.contains("args") || result.message.contains("not found"),
            "unexpected: {}",
            result.message
        );
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
            orphan_dirs: 0,
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
            orphan_dirs: 0,
            loaded: true,
        };
        let checks = build_registry_checks(&info, None);
        assert!(checks[0].message.contains("5 registered"));
        // Details should show stale count
        let details_str = checks[0].details.join(" ");
        assert!(details_str.contains("stale: 2"));
        assert!(checks[0].suggestion.as_ref().unwrap().contains("prune"));
    }

    #[tokio::test]
    async fn test_build_registry_checks_no_stale_no_hint() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 2,
            reachable_projects: 2,
            orphan_dirs: 0,
            loaded: true,
        };
        let checks = build_registry_checks(&info, None);
        let details_str = checks[0].details.join(" ");
        assert!(!details_str.contains("stale"));
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
        assert_eq!(project_section.checks[0].name, "Store initialized");
        assert!(!project_section.checks[0].passed);
        assert!(project_section.checks[0].message.contains("not found"));
    }

    // --- Group 5: new health checks ---

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(
            format_bytes(1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "1.5 GB"
        );
    }

    #[tokio::test]
    async fn test_dir_size_empty() {
        let temp_dir = TempDir::new().unwrap();
        let size = dir_size(temp_dir.path());
        assert_eq!(size, 0);
    }

    #[tokio::test]
    async fn test_dir_size_with_files() {
        let temp_dir = TempDir::new().unwrap();
        async_fs::write(temp_dir.path().join("file1.txt"), "hello")
            .await
            .unwrap();
        async_fs::write(temp_dir.path().join("file2.txt"), "world!")
            .await
            .unwrap();

        let size = dir_size(temp_dir.path());
        assert_eq!(size, 11); // "hello" (5) + "world!" (6)
    }

    #[tokio::test]
    async fn test_dir_size_nonexistent() {
        let size = dir_size(Path::new("/nonexistent/path/abc123"));
        assert_eq!(size, 0);
    }

    #[tokio::test]
    async fn test_dir_size_nested() {
        let temp_dir = TempDir::new().unwrap();
        let sub = temp_dir.path().join("sub");
        async_fs::create_dir_all(&sub).await.unwrap();
        async_fs::write(temp_dir.path().join("a.txt"), "aaa")
            .await
            .unwrap();
        async_fs::write(sub.join("b.txt"), "bbbbb").await.unwrap();

        let size = dir_size(temp_dir.path());
        assert_eq!(size, 8); // 3 + 5
    }

    #[tokio::test]
    async fn test_dir_stats_counts_and_sizes_with_extension_filter() {
        use rand::Rng;

        let temp_dir = TempDir::new().unwrap();
        let mut rng = rand::rng();
        let file_count: usize = rng.random_range(1..=100);
        let mut expected_md_size = 0u64;
        let mut expected_md_count = 0usize;
        let mut expected_total_size = 0u64;

        for i in 0..file_count {
            let size: usize = rng.random_range(1024..=4096);
            let data = vec![0u8; size];
            let ext = if i % 3 == 0 { "txt" } else { "md" };
            let path = temp_dir.path().join(format!("file_{}.{}", i, ext));
            std::fs::write(&path, &data).unwrap();
            expected_total_size += size as u64;
            if ext == "md" {
                expected_md_size += size as u64;
                expected_md_count += 1;
            }
        }

        let all = dir_stats(temp_dir.path(), None);
        assert_eq!(all.total_size, expected_total_size);
        assert_eq!(all.file_count, file_count);

        let md_only = dir_stats(temp_dir.path(), Some("md"));
        assert_eq!(md_only.total_size, expected_md_size);
        assert_eq!(md_only.file_count, expected_md_count);

        // dir_size should match unfiltered total
        assert_eq!(dir_size(temp_dir.path()), expected_total_size);
    }

    #[tokio::test]
    async fn test_dir_stats_nested_directories() {
        use rand::Rng;

        let temp_dir = TempDir::new().unwrap();
        let mut rng = rand::rng();
        let mut expected_size = 0u64;
        let mut expected_count = 0usize;

        // Create nested structure: root/sub1/sub2/
        let sub1 = temp_dir.path().join("sub1");
        let sub2 = sub1.join("sub2");
        std::fs::create_dir_all(&sub2).unwrap();

        for (dir, prefix) in [
            (temp_dir.path(), "root"),
            (sub1.as_path(), "sub1"),
            (sub2.as_path(), "sub2"),
        ] {
            let count: usize = rng.random_range(1..=10);
            for i in 0..count {
                let size: usize = rng.random_range(1024..=4096);
                let data = vec![0u8; size];
                std::fs::write(dir.join(format!("{}_{}.md", prefix, i)), &data).unwrap();
                expected_size += size as u64;
                expected_count += 1;
            }
        }

        let stats = dir_stats(temp_dir.path(), Some("md"));
        assert_eq!(stats.total_size, expected_size);
        assert_eq!(stats.file_count, expected_count);
    }

    #[tokio::test]
    async fn test_subdir_sizes_reports_per_directory() {
        use rand::Rng;

        let temp_dir = TempDir::new().unwrap();
        let mut rng = rand::rng();
        let mut expected: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

        // Create 3-5 subdirectories with random files
        let subdir_count: usize = rng.random_range(3..=5);
        for d in 0..subdir_count {
            let name = format!("dir_{}", d);
            let subdir = temp_dir.path().join(&name);
            std::fs::create_dir(&subdir).unwrap();

            let file_count: usize = rng.random_range(1..=20);
            let mut dir_total = 0u64;
            for f in 0..file_count {
                let size: usize = rng.random_range(1024..=4096);
                let data = vec![0u8; size];
                std::fs::write(subdir.join(format!("file_{}.bin", f)), &data).unwrap();
                dir_total += size as u64;
            }
            expected.insert(name, dir_total);
        }

        let results = subdir_sizes(temp_dir.path());
        assert_eq!(results.len(), subdir_count);

        for (name, size) in &results {
            let exp = expected
                .get(name)
                .unwrap_or_else(|| panic!("unexpected dir: {}", name));
            assert_eq!(size, exp, "size mismatch for {}", name);
        }
    }

    #[tokio::test]
    async fn test_dir_stats_empty_returns_zeros() {
        let temp_dir = TempDir::new().unwrap();
        let stats = dir_stats(temp_dir.path(), None);
        assert_eq!(stats.total_size, 0);
        assert_eq!(stats.file_count, 0);
    }

    #[tokio::test]
    async fn test_dir_stats_nonexistent_returns_zeros() {
        let stats = dir_stats(Path::new("/nonexistent/dir/abc"), Some("md"));
        assert_eq!(stats.total_size, 0);
        assert_eq!(stats.file_count, 0);
    }

    #[tokio::test]
    async fn test_subdir_sizes_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let results = subdir_sizes(temp_dir.path());
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_subdir_sizes_nonexistent() {
        let results = subdir_sizes(Path::new("/nonexistent/dir/abc"));
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_manifest_stats_in_sync() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        let result = check_manifest_stats(temp_dir.path(), &store).await;
        assert_eq!(result.name, "Manifest stats");
        assert!(result.passed);
        assert!(result.message.contains("1"));
    }

    #[tokio::test]
    async fn test_check_manifest_stats_drift() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        // Corrupt manifest stats
        let manifest_path = temp_dir.path().join(".engramdb").join("manifest.toml");
        let mut manifest = crate::storage::manifest::load_manifest(&manifest_path)
            .await
            .unwrap();
        manifest.stats.memory_count = 99;
        crate::storage::manifest::save_manifest(&manifest_path, &manifest)
            .await
            .unwrap();

        let result = check_manifest_stats(temp_dir.path(), &store).await;
        assert_eq!(result.status, Some(CheckStatus::Warn));
        assert!(result.message.contains("99"));
        assert!(result.message.contains("1"));
    }

    #[tokio::test]
    async fn test_check_write_lock_no_file() {
        // Use a fake project_id that won't have a lock file
        let result = check_write_lock("nonexistent-project-id-12345").await;
        assert_eq!(result.name, "Write lock");
        assert!(result.passed);
    }

    #[tokio::test]
    async fn test_check_chunk_orphans_clean() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        let result = check_chunk_orphans(&store).await;
        assert_eq!(result.name, "Chunk index integrity");
        assert!(result.passed);
    }

    #[tokio::test]
    async fn test_check_chunk_orphans_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = check_chunk_orphans(&store).await;
        assert!(result.passed);
        assert!(result.message.contains("0 chunk"));
    }

    #[tokio::test]
    async fn test_check_global_disk_usage_always_passes() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_global_disk_usage(temp_dir.path(), temp_dir.path()).await;
        assert_eq!(result.name, "Global disk usage");
        assert!(result.passed);
        // Message is now the total size; details have the breakdown
        let details_str = result.details.join(" ");
        assert!(details_str.contains("projects:"));
        assert!(details_str.contains("models:"));
    }

    #[tokio::test]
    async fn test_check_project_disk_usage_always_passes() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_project_disk_usage(temp_dir.path(), "fake-project-id").await;
        assert_eq!(result.name, "Project disk usage");
        assert!(result.passed);
        // Message is now the total size; details have the breakdown
        let details_str = result.details.join(" ");
        assert!(details_str.contains("shared:"));
        assert!(details_str.contains("memories"));
        assert!(details_str.contains("index:"));
        assert!(details_str.contains("personal:"));
    }

    #[tokio::test]
    async fn test_environment_has_new_checks_with_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store)).await;
        let names: Vec<&str> = result
            .all_checks()
            .iter()
            .map(|c| c.name.as_str())
            .collect();

        assert!(
            names.contains(&"Manifest stats"),
            "missing Manifest stats check"
        );
        assert!(
            names.contains(&"Chunk index integrity"),
            "missing Chunk index integrity check"
        );
        assert!(
            names.contains(&"Global disk usage"),
            "missing Global disk usage check"
        );
    }

    #[tokio::test]
    async fn test_environment_no_store_skips_gated_checks() {
        let temp_dir = TempDir::new().unwrap();

        let result = doctor_environment(temp_dir.path(), None).await;
        let names: Vec<&str> = result
            .all_checks()
            .iter()
            .map(|c| c.name.as_str())
            .collect();

        // Gated checks should not appear without a store
        assert!(
            !names.contains(&"Manifest stats"),
            "Manifest stats should not appear without store"
        );
        assert!(
            !names.contains(&"Chunk index integrity"),
            "Chunk index integrity should not appear without store"
        );
        // Global disk usage is always present
        assert!(names.contains(&"Global disk usage"));
    }

    #[test]
    fn test_check_project_gitignore_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_project_gitignore(temp_dir.path());
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Info));
        assert!(result.message.contains("no .gitignore"));
    }

    #[test]
    fn test_check_project_gitignore_ignored() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".gitignore"), ".engramdb\n").unwrap();
        let result = check_project_gitignore(temp_dir.path());
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Info));
        assert!(result.message.contains("ignored by project"));
    }

    #[test]
    fn test_check_project_gitignore_negated() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join(".gitignore"),
            ".engramdb\n!.engramdb/\n",
        )
        .unwrap();
        let result = check_project_gitignore(temp_dir.path());
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Info));
        assert!(result.message.contains("explicitly included"));
    }

    #[test]
    fn test_check_project_gitignore_not_mentioned() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        let result = check_project_gitignore(temp_dir.path());
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Info));
        assert!(result.message.contains("not mentioned"));
    }

    #[test]
    fn test_check_global_gitignore_runs() {
        // Just verify it doesn't panic — actual global state varies
        let result = check_global_gitignore();
        assert!(result.passed);
    }

    #[tokio::test]
    async fn test_environment_has_gitignore_section() {
        let temp_dir = TempDir::new().unwrap();
        let result = doctor_environment(temp_dir.path(), None).await;
        let section_names: Vec<&str> = result.sections.iter().map(|s| s.name.as_str()).collect();
        assert!(
            section_names.contains(&"Gitignore"),
            "missing Gitignore section"
        );
    }
}
