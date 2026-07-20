//! Store health check operation.

use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;
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
                let id = crate::storage::memory_file::extract_id_from_stem(stem);
                if !indexed_ids.contains(id) {
                    orphaned.push(id.to_string());
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subsections: Vec<DoctorSubSection>,
}

/// A named sub-group within a section.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoctorSubSection {
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
    /// Flatten all checks from all sections and subsections into a single vec.
    pub fn all_checks(&self) -> Vec<&EnvironmentCheck> {
        self.sections
            .iter()
            .flat_map(|s| {
                s.checks
                    .iter()
                    .chain(s.subsections.iter().flat_map(|ss| &ss.checks))
            })
            .collect()
    }
}

/// Run a full environment diagnostic organized into sections.
///
/// Sections, in order: Project (current project's stats & health),
/// Projects (all registered projects), Global settings & models
/// (binaries, integration, embeddings, the active models, and the daemon),
/// and Stats (global disk usage).
pub async fn doctor_environment(
    dir: &Path,
    store: Option<&MemoryStore>,
    daemon_check: EnvironmentCheck,
) -> EnvironmentDoctorResult {
    let mut sections = Vec::new();

    // --- Load registry once for Project gating and the Projects section ---
    let registry_info = load_registry_info(dir).await;
    let store_initialized = dir.join(".engramdb").exists();

    // When this project has no EngramDB setup, none of the project-level
    // checks apply and the global sections (registry, models, stats) are noise
    // for someone who just wants to know "is this set up?". Show only the
    // single "not set up" notice and stop.
    if !store_initialized {
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
            subsections: vec![],
        });
        return EnvironmentDoctorResult {
            sections,
            all_passed: false,
            store_check: None,
        };
    }

    // === 1. Project section (current project's stats & health) ===
    let store_path = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .join(".engramdb");

    let store_check = {
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

        // Second clone of the same git remote: both checkouts hash to the
        // same project ID, so they share one LanceDB index, write lock, and
        // personal-memories dir while keeping separate .engramdb/memories/.
        if let Some(other) = &registry_info.conflicting_checkout {
            project_checks.push(EnvironmentCheck {
                name: "Checkout identity".to_string(),
                passed: true,
                message: format!(
                    "project ID {} is shared with another checkout at {}",
                    project_id,
                    other.display()
                ),
                suggestion: Some(
                    "Two checkouts of the same remote share one index; memories created \
                     in the other checkout appear as stale entries here, and reindex runs \
                     in non-destructive mode. Prefer running engramdb from the registered \
                     checkout, or remove it and run `engramdb init` here to take over."
                        .to_string(),
                ),
                details: vec![],
                status: Some(CheckStatus::Warn),
            });
        }

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
            project_checks.push(check_project_gitignore(dir));
            project_checks.push(check_project_disk_usage(dir, &project_id).await);
        } else {
            project_checks.push(EnvironmentCheck {
                name: "Registry".to_string(),
                passed: true,
                message: "not registered".to_string(),
                suggestion: Some("Run `engramdb init` to register this project".to_string()),
                details: vec![],
                status: Some(CheckStatus::Warn),
            });
        }
        // Group subscriptions (multi-project memories): one check per subscribed
        // group — readability + embedding-fingerprint alignment. Omitted
        // entirely when the project subscribes to no groups.
        let group_checks = check_subscribed_groups(dir).await;
        let project_subsections = if group_checks.is_empty() {
            vec![]
        } else {
            vec![DoctorSubSection {
                name: "Group subscriptions".to_string(),
                checks: group_checks,
            }]
        };

        sections.push(DoctorSection {
            name: "Project".to_string(),
            checks: project_checks,
            subsections: project_subsections,
        });
        sc
    };

    // === 2. Projects section (all registered projects) ===
    sections.push(DoctorSection {
        name: "Projects".to_string(),
        checks: build_registry_checks(&registry_info),
        subsections: vec![],
    });

    // === 3. Global settings & models ===
    //
    // Machine-wide settings (binary, global gitignore, agent integration,
    // auto-maintenance status), the embedding config/health, the active models
    // with a short description of what each is used for, and the optional
    // shared inference daemon. The
    // daemon probe is built by the caller (`daemon::doctor::check_daemon`) and
    // injected here so that `ops` does not depend "upward" on `daemon`. The
    // daemon is optional and auto-spawned, so it never counts as a failure.
    let global_checks = vec![
        check_binary_on_path().await,
        check_global_gitignore(),
        check_claude_plugin(),
        check_hook_config(),
        check_maintenance(dir).await,
        daemon_check,
    ];

    let mut embeddings_checks = Vec::new();
    embeddings_checks.push(check_embedding_backend(dir).await);
    embeddings_checks.push(check_embedding_model_identity(dir).await);
    let cache_dir = crate::storage::paths::model_cache_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from(".cache/engramdb/models"));
    embeddings_checks.push(check_embedding_model_cached(dir, &cache_dir).await);
    #[cfg(feature = "ollama")]
    embeddings_checks.push(check_ollama_connectivity(dir).await);

    sections.push(DoctorSection {
        name: "Global settings & models".to_string(),
        checks: global_checks,
        subsections: vec![
            DoctorSubSection {
                name: "Embeddings".to_string(),
                checks: embeddings_checks,
            },
            DoctorSubSection {
                name: "Models".to_string(),
                checks: check_active_models(dir).await,
            },
        ],
    });

    // === 4. Stats section (global disk usage) ===
    let stats_check = match (
        crate::storage::paths::global_data_dir(),
        crate::storage::paths::model_cache_dir(),
    ) {
        (Ok(data_dir), Ok(cache_dir)) => check_global_disk_usage(&data_dir, &cache_dir).await,
        _ => EnvironmentCheck {
            name: "Global disk usage".to_string(),
            passed: false,
            message: "could not determine global data/cache directories".to_string(),
            suggestion: Some("Check platform directory configuration".to_string()),
            details: vec![],
            status: None,
        },
    };
    sections.push(DoctorSection {
        name: "Stats".to_string(),
        checks: vec![stats_check],
        subsections: vec![],
    });

    let all_passed = sections
        .iter()
        .flat_map(|s| {
            s.checks
                .iter()
                .chain(s.subsections.iter().flat_map(|ss| &ss.checks))
        })
        .all(|c| c.passed);

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
        // Advisory: the MCP server and Claude Code hooks invoke an absolute
        // binary path, so `engramdb` not being on PATH breaks nothing. Render
        // it as a warning, not a failure that would flip the exit code.
        _ => EnvironmentCheck {
            name: "Binary on PATH".to_string(),
            passed: true,
            message: "not found".to_string(),
            suggestion: Some("Install with `brew install engramdb`".to_string()),
            details: vec![],
            status: Some(CheckStatus::Warn),
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
    hierarchy_dangling: usize,
    hierarchy_stale_parent: usize,
    hierarchy_cycle: usize,
    /// A different, still-existing checkout is registered as the owner of
    /// this project ID (second clone of the same git remote). The two
    /// checkouts share one LanceDB index, write lock, and personal-memories
    /// dir while keeping separate `.engramdb/memories/` trees.
    conflicting_checkout: Option<PathBuf>,
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
                hierarchy_dangling: 0,
                hierarchy_stale_parent: 0,
                hierarchy_cycle: 0,
                conflicting_checkout: None,
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

            let issues = crate::ops::projects::scan_hierarchy_issues(&reg);

            let conflicting_checkout =
                crate::storage::conflicting_checkout_path(&reg, &project_id, dir);

            RegistryInfo {
                in_registry,
                total_projects,
                reachable_projects,
                orphan_dirs,
                loaded: true,
                hierarchy_dangling: issues.dangling.len(),
                hierarchy_stale_parent: issues.stale_parent.len(),
                hierarchy_cycle: issues.cycle_members.len(),
                conflicting_checkout,
            }
        }
        Err(_) => RegistryInfo {
            in_registry: false,
            total_projects: 0,
            reachable_projects: 0,
            orphan_dirs: 0,
            loaded: false,
            hierarchy_dangling: 0,
            hierarchy_stale_parent: 0,
            hierarchy_cycle: 0,
            conflicting_checkout: None,
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

/// Check whether the store's stored embedding fingerprint matches the
/// model the current config would use. A mismatch/untracked store means
/// search is served from stale or mixed vectors until a reindex.
async fn check_embedding_model_identity(dir: &Path) -> EnvironmentCheck {
    use crate::storage::{embedding_status, EmbeddingModelStatus};

    let name = "Embedding model identity".to_string();
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;
    let manifest_path = dir.join(".engramdb").join("manifest.toml");
    let stored = crate::storage::manifest::load_manifest(&manifest_path)
        .await
        .ok()
        .and_then(|m| m.embedding);

    let Some(expected) = super::expected_embedding_fingerprint(&config) else {
        return EnvironmentCheck {
            name,
            passed: true,
            message: "no ONNX/Ollama embedding model resolved from config".to_string(),
            suggestion: None,
            details: vec![],
            status: None,
        };
    };

    let reindex = "run `engramdb reindex --embeddings-only` to re-embed and stamp the store";
    // `Untracked` is advisory: the fingerprint is only stamped by `reindex`,
    // never by `add`, so every normally-used store is "untracked (legacy
    // store)" until its first reindex even though semantic search works fine.
    // Render it as a warning so it never flips the exit code. `Mismatch` and
    // `DimensionMismatch` are genuine correctness bugs (search served from
    // stale/mixed vectors) and stay hard failures.
    let (passed, message, suggestion, status) = match embedding_status(
        stored.as_ref(),
        &expected.model,
        expected.dimensions,
        expected.composition.as_deref(),
    ) {
        EmbeddingModelStatus::Match => (true, format!("ok: {}", expected.model), None, None),
        EmbeddingModelStatus::Untracked { current } => (
            true,
            format!("untracked (legacy store); current model {current}"),
            Some(reindex.to_string()),
            Some(CheckStatus::Warn),
        ),
        EmbeddingModelStatus::Mismatch { stored, current } => (
            false,
            format!(
                "MISMATCH: stored {stored}, current {current} — search uses stale/mixed vectors"
            ),
            Some(reindex.to_string()),
            None,
        ),
        EmbeddingModelStatus::DimensionMismatch { stored, current } => (
            false,
            format!("DIMENSION MISMATCH: stored {stored}d vs current {current}d"),
            Some(reindex.to_string()),
            None,
        ),
        // Advisory like Untracked: old vectors still work, they just lack the
        // metadata row (or unexpectedly carry one) — ranking skew, not
        // corruption. Warn without flipping the exit code.
        EmbeddingModelStatus::CompositionMismatch { stored, current } => (
            true,
            format!(
                "composition changed: stored {}, current {} — old memories rank \
                 without title/tag signal",
                stored.as_deref().unwrap_or("legacy"),
                current.as_deref().unwrap_or("legacy")
            ),
            Some(reindex.to_string()),
            Some(CheckStatus::Warn),
        ),
    };
    EnvironmentCheck {
        name,
        passed,
        message,
        suggestion,
        details: vec![],
        status,
    }
}

/// For each group this project subscribes to, check that the group store is
/// readable and that its embedding fingerprint aligns with the project's own.
///
/// A **drift** (the group embedded its vectors with a different model than the
/// project) means cross-store scores are computed by different models and are
/// not directly comparable — the merged ranking can be skewed. P2's post-merge
/// rerank equalization is the model-agnostic fix; until then, reindexing the
/// group (or the project) onto a shared embedding model realigns them. An
/// **unreadable** subscribed store means that group's memories silently vanish
/// from this project's queries, so it is surfaced here rather than only logged
/// on the hot path. Both are advisory (never flip the exit code): the project's
/// own operation is unaffected.
///
/// Returns an empty vec when the project subscribes to no groups, so the caller
/// can omit the whole subsection.
async fn check_subscribed_groups(dir: &Path) -> Vec<EnvironmentCheck> {
    use crate::storage::registry::subscriptions_of;
    use crate::storage::RegistryBackend;

    let project_id = crate::storage::project_id::compute_project_id(dir);
    let Some(reg) = (match crate::storage::FileRegistry::global() {
        Ok(r) => r.load().await.ok(),
        Err(_) => None,
    }) else {
        return Vec::new();
    };
    let subs = subscriptions_of(&reg, &project_id);
    if subs.is_empty() {
        return Vec::new();
    }

    // The project's own expected embedding fingerprint — what its vectors use,
    // and the yardstick each subscribed group is compared against. `None` when
    // the project resolves no embedding model (keyword-only): then there is
    // nothing to drift from, so we report readability only.
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;
    let project_fp = super::expected_embedding_fingerprint(&config);

    let mut checks = Vec::new();
    for gid in subs {
        let name = reg
            .groups
            .iter()
            .find(|g| &g.group_id == gid)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| gid.clone());
        checks.push(check_one_group(gid, &name, project_fp.as_ref()).await);
    }
    checks
}

/// Readability + fingerprint-alignment check for a single subscribed group.
/// See [`check_subscribed_groups`] for the rationale and severities.
async fn check_one_group(
    gid: &str,
    name: &str,
    project_fp: Option<&crate::storage::manifest::EmbeddingFingerprint>,
) -> EnvironmentCheck {
    use crate::storage::{embedding_status, EmbeddingModelStatus};

    let check_name = format!("Group '{name}'");
    let store_dir = match crate::storage::paths::group_store_dir(gid) {
        Ok(d) => d,
        Err(e) => {
            return EnvironmentCheck {
                name: check_name,
                passed: false,
                message: format!("cannot resolve group store path: {e}"),
                suggestion: None,
                details: vec![],
                status: Some(CheckStatus::Warn),
            };
        }
    };
    let engramdb_dir = crate::storage::paths::project_dir(&store_dir);

    // Never written to → empty, not a problem (and don't create it just to
    // count). This is the empty side of the empty-vs-corrupt distinction.
    if !engramdb_dir.exists() {
        return EnvironmentCheck {
            name: check_name,
            passed: true,
            message: format!("subscribed (id: {gid}); no memories yet"),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
        };
    }

    // Readability: a store dir that exists but won't open/count is corrupt —
    // its memories silently drop out of fan-in.
    let count = match crate::storage::MemoryStore::open_group(gid).await {
        Ok(store) => store.count().await.unwrap_or(0),
        Err(e) => {
            return EnvironmentCheck {
                name: check_name,
                passed: true,
                message: format!("UNREADABLE (id: {gid}): {e}"),
                suggestion: Some(
                    "the group store is corrupt; its memories drop out of this project's \
                     queries. Recreate or reindex the group store."
                        .to_string(),
                ),
                details: vec![],
                status: Some(CheckStatus::Warn),
            };
        }
    };

    // Fingerprint alignment. With no project-side model resolved there's
    // nothing to compare — report readability only.
    let Some(pfp) = project_fp else {
        return EnvironmentCheck {
            name: check_name,
            passed: true,
            message: format!("subscribed (id: {gid}); {count} memories"),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
        };
    };

    let group_manifest = engramdb_dir.join("manifest.toml");
    let group_fp = crate::storage::manifest::load_manifest(&group_manifest)
        .await
        .ok()
        .and_then(|m| m.embedding);

    let realign = "reindex the group (or the project) onto a shared embedding model, or rely on \
                   the post-merge rerank pass to equalize scores";
    let (passed, message, suggestion, status) = match embedding_status(
        group_fp.as_ref(),
        &pfp.model,
        pfp.dimensions,
        pfp.composition.as_deref(),
    ) {
        EmbeddingModelStatus::Match => (
            true,
            format!("aligned on {}; {count} memories", pfp.model),
            None,
            Some(CheckStatus::Pass),
        ),
        // The group store predates fingerprint stamping. Its vectors may or may
        // not match; we can't tell, so advise a reindex but don't alarm.
        EmbeddingModelStatus::Untracked { .. } => (
            true,
            format!("group store not fingerprinted (legacy); {count} memories"),
            Some("run `engramdb reindex --embeddings-only` on the group to stamp it".to_string()),
            Some(CheckStatus::Info),
        ),
        EmbeddingModelStatus::Mismatch { stored, current } => (
            true,
            format!(
                "MODEL DRIFT: group embedded with {stored}, project uses {current} — cross-store \
                 ranking may be skewed ({count} memories)"
            ),
            Some(realign.to_string()),
            Some(CheckStatus::Warn),
        ),
        EmbeddingModelStatus::DimensionMismatch { stored, current } => (
            true,
            format!(
                "DIMENSION DRIFT: group {stored}d vs project {current}d — cross-store ranking may \
                 be skewed ({count} memories)"
            ),
            Some(realign.to_string()),
            Some(CheckStatus::Warn),
        ),
        // Composition-only drift (title/tag signal) is minor ranking skew, not
        // model incompatibility.
        EmbeddingModelStatus::CompositionMismatch { .. } => (
            true,
            format!(
                "composition differs from project's; minor cross-store ranking skew ({count} \
                 memories)"
            ),
            Some(realign.to_string()),
            Some(CheckStatus::Info),
        ),
    };
    EnvironmentCheck {
        name: check_name,
        passed,
        message,
        suggestion,
        details: vec![],
        status,
    }
}

/// Render a duration as a coarse "N unit(s)" string, picking the single largest
/// whole unit. Diagnostics only, so it favors readability over precision.
fn humanize_interval(d: Duration) -> String {
    let secs = d.as_secs();
    let (value, unit) = if secs < 60 {
        (secs, "second")
    } else if secs < 3600 {
        (secs / 60, "minute")
    } else if secs < 86_400 {
        (secs / 3600, "hour")
    } else {
        (secs / 86_400, "day")
    };
    format!("{value} {unit}{}", if value == 1 { "" } else { "s" })
}

/// Report auto-maintenance status (the `[maintenance]` section): whether the
/// throttled main-worktree housekeeping pass is enabled, its interval, and when
/// it last ran. Always informational — auto-maintenance is best-effort, so it
/// never counts as a failure.
async fn check_maintenance(dir: &Path) -> EnvironmentCheck {
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;
    let status = crate::ops::maintenance_status(&config.maintenance).await;

    if !status.enabled {
        return EnvironmentCheck {
            name: "Auto-maintenance".to_string(),
            passed: true,
            message: "disabled".to_string(),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
        };
    }

    let last_run = match status.last_run {
        Some(t) => t
            .elapsed()
            .map(|e| format!("{} ago", humanize_interval(e)))
            .unwrap_or_else(|_| "just now".to_string()),
        None => "never".to_string(),
    };

    EnvironmentCheck {
        name: "Auto-maintenance".to_string(),
        passed: true,
        message: format!("enabled (every {})", humanize_interval(status.interval)),
        suggestion: None,
        details: vec![format!("last run: {last_run}")],
        status: Some(CheckStatus::Info),
    }
}

/// Informational check showing the active embedding backend and model from config.
async fn check_embedding_backend(dir: &Path) -> EnvironmentCheck {
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;

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

/// Describe each model EngramDB will load for the current config: which
/// model it is and what it is used for.
///
/// All entries are informational (`CheckStatus::Info`) — they report the
/// configured setup, never a pass/fail. The embedding model is always
/// listed; the reranker, NLI, and title models only appear when their
/// feature is enabled in config (otherwise no model is loaded for them).
async fn check_active_models(dir: &Path) -> Vec<EnvironmentCheck> {
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;

    let info = |name: &str, message: String, detail: &str| EnvironmentCheck {
        name: name.to_string(),
        passed: true,
        message,
        suggestion: None,
        details: vec![detail.to_string()],
        status: Some(CheckStatus::Info),
    };

    let mut checks = Vec::new();

    // Embedding model — always loaded; powers semantic search.
    let embed_model = super::expected_embedding_fingerprint(&config)
        .map(|fp| fp.model)
        .unwrap_or_else(|| config.embeddings.provider.clone());
    checks.push(info(
        "Embedding model",
        format!("{embed_model} ({}d)", config.embeddings.dimensions),
        "Local ONNX bi-encoder; embeds memories and queries into vectors so \
         search can rank by semantic similarity.",
    ));

    // Reranker — optional; refines the initial ranking.
    if config.rerank.enabled {
        checks.push(info(
            "Reranker model",
            config.rerank.model.clone(),
            "Cross-encoder that re-scores the top candidates by jointly reading \
             query and document — slower but sharper relevance than the bi-encoder.",
        ));
    }

    // NLI — optional; drives the challenge/contradiction flow.
    if config.nli.enabled {
        checks.push(info(
            "NLI model",
            config.nli.model.clone(),
            "Natural-language-inference model; flags memories that contradict \
             each other for the `challenge` review flow.",
        ));
    }

    // Title generation — keyword needs no model; T5 loads one; none disables it.
    match config.title.strategy {
        crate::types::TitleStrategy::T5 => checks.push(info(
            "Title model",
            "Xenova/t5-small (int8)".to_string(),
            "T5-small abstractive summarizer; generates a memory title when the \
             caller doesn't supply one.",
        )),
        crate::types::TitleStrategy::Keyword => checks.push(info(
            "Title generation",
            "keyword (RAKE)".to_string(),
            "In-process RAKE keyword extraction; no model is loaded.",
        )),
        crate::types::TitleStrategy::None => checks.push(info(
            "Title generation",
            "disabled".to_string(),
            "Automatic title generation is off; no model is loaded.",
        )),
    }

    checks
}

/// Load every model the current config would use (plus any optional model
/// that is already downloaded) and run a tiny inference to confirm it works.
///
/// Returns one [`EnvironmentCheck`] per model role:
/// - `Pass` — the model loaded and a test inference succeeded.
/// - `Fail` — the model is present/enabled but loading or inference failed.
/// - `Info` — nothing to validate (the model is neither enabled nor
///   downloaded), so it is skipped.
///
/// A model that is enabled in config *or* already downloaded is exercised, so
/// a model you pulled but haven't switched on yet is still checked. Download
/// detection uses the HuggingFace cache layout for hub models (embedding, NLI,
/// T5); the fastembed reranker is keyed off `[rerank].enabled` because its
/// cache layout is not probed here.
pub async fn validate_models(config: &crate::types::EngramConfig) -> Vec<EnvironmentCheck> {
    use std::time::Instant;

    let mut vcfg = config.clone();
    vcfg.nli.enabled =
        config.nli.enabled || crate::storage::paths::hf_repo_cached(&config.nli.model);
    // T5 titling is ONNX-Runtime-only; on a pure-`tract` build the `t5` module
    // is compiled out, so only probe for it when ORT is present.
    #[cfg(feature = "onnxruntime")]
    if config.title.strategy != crate::title::TitleStrategy::T5
        && crate::storage::paths::hf_repo_cached(crate::title::t5::DEFAULT_T5_MODEL.repo)
    {
        vcfg.title.strategy = crate::title::TitleStrategy::T5;
    }

    // One heavyweight load of the whole bundle (embedding always; the others
    // per the flags above). The CLI is one-shot, so a single session each.
    let providers = super::resolve_engine_providers(&vcfg, None, 1);

    let pass = |name: &str, message: String| EnvironmentCheck {
        name: name.to_string(),
        passed: true,
        message,
        suggestion: None,
        details: vec![],
        status: Some(CheckStatus::Pass),
    };
    let fail = |name: &str, message: String, hint: &str| EnvironmentCheck {
        name: name.to_string(),
        passed: false,
        message,
        suggestion: Some(hint.to_string()),
        details: vec![],
        status: None,
    };
    let skip = |name: &str, message: String| EnvironmentCheck {
        name: name.to_string(),
        passed: true,
        message,
        suggestion: None,
        details: vec![],
        status: Some(CheckStatus::Info),
    };

    let mut checks = Vec::new();

    // Embedding — always loaded.
    match &providers.embedding {
        Some(p) => {
            let t = Instant::now();
            match p.embed("EngramDB model validation probe").await {
                Ok(v)
                    if !v.is_empty()
                        && v.len() == p.dimensions()
                        && v.iter().all(|f| f.is_finite()) =>
                {
                    checks.push(pass(
                        "Embedding model",
                        format!(
                            "ok — {} ({}d) in {}ms",
                            p.model_id(),
                            v.len(),
                            t.elapsed().as_millis()
                        ),
                    ))
                }
                Ok(v) => checks.push(fail(
                    "Embedding model",
                    format!(
                        "produced an unusable vector ({} dims, expected {})",
                        v.len(),
                        p.dimensions()
                    ),
                    "Run `engramdb reindex --embeddings-only` after fixing the model",
                )),
                Err(e) => checks.push(fail(
                    "Embedding model",
                    format!("inference failed: {e}"),
                    "Re-download the embedding model with `engramdb init`",
                )),
            }
        }
        None => checks.push(fail(
            "Embedding model",
            "failed to load".to_string(),
            "Run `engramdb init` to (re)download the embedding model",
        )),
    }

    // NLI — enabled or downloaded.
    if vcfg.nli.enabled {
        match &providers.nli {
            Some(p) => {
                let t = Instant::now();
                match p.classify("The sky is blue.", "The sky is not blue.").await {
                    Ok(_) => checks.push(pass(
                        "NLI model",
                        format!("ok — {} in {}ms", config.nli.model, t.elapsed().as_millis()),
                    )),
                    Err(e) => checks.push(fail(
                        "NLI model",
                        format!("inference failed: {e}"),
                        "Re-download the NLI model",
                    )),
                }
            }
            None => checks.push(fail(
                "NLI model",
                "present but failed to load".to_string(),
                "Re-download the NLI model",
            )),
        }
    } else {
        checks.push(skip(
            "NLI model",
            "not enabled and not downloaded (skipped)".to_string(),
        ));
    }

    // Reranker — keyed off config (cache layout not probed here).
    if config.rerank.enabled {
        match &providers.reranker {
            Some(p) => {
                let t = Instant::now();
                let docs = vec![
                    "a relevant document about memory stores".to_string(),
                    "unrelated text".to_string(),
                ];
                match p.rerank("memory store health", &docs).await {
                    Ok(_) => checks.push(pass(
                        "Reranker model",
                        format!(
                            "ok — {} in {}ms",
                            config.rerank.model,
                            t.elapsed().as_millis()
                        ),
                    )),
                    Err(e) => checks.push(fail(
                        "Reranker model",
                        format!("inference failed: {e}"),
                        "Re-download the reranker model",
                    )),
                }
            }
            None => checks.push(fail(
                "Reranker model",
                "enabled but failed to load".to_string(),
                "Re-download the reranker model",
            )),
        }
    } else {
        checks.push(skip(
            "Reranker model",
            "disabled in config (skipped)".to_string(),
        ));
    }

    // T5 title — enabled or downloaded.
    if vcfg.title.strategy == crate::title::TitleStrategy::T5 {
        match &providers.title {
            Some(p) => {
                let t = Instant::now();
                match p
                    .generate(
                        "EngramDB is a project-scoped persistent memory store for coding agents.",
                    )
                    .await
                {
                    Ok(_) => checks.push(pass(
                        "Title model",
                        format!("ok — Xenova/t5-small in {}ms", t.elapsed().as_millis()),
                    )),
                    Err(e) => checks.push(fail(
                        "Title model",
                        format!("inference failed: {e}"),
                        "Re-download the T5 title model",
                    )),
                }
            }
            None => checks.push(fail(
                "Title model",
                "present but failed to load".to_string(),
                "Re-download the T5 title model",
            )),
        }
    } else {
        checks.push(skip(
            "Title model",
            "keyword titling (no model) — skipped".to_string(),
        ));
    }

    checks
}

/// Check Ollama connectivity with a 3-second timeout.
#[cfg(feature = "ollama")]
async fn check_ollama_connectivity(dir: &Path) -> EnvironmentCheck {
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;
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
fn build_registry_checks(info: &RegistryInfo) -> Vec<EnvironmentCheck> {
    let mut checks = Vec::new();

    if !info.loaded {
        checks.push(EnvironmentCheck {
            name: "Registered projects".to_string(),
            passed: true,
            message: "registry unavailable".to_string(),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Warn),
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

    let total_hierarchy_issues =
        info.hierarchy_dangling + info.hierarchy_stale_parent + info.hierarchy_cycle;
    if total_hierarchy_issues > 0 {
        let mut h_details = Vec::new();
        if info.hierarchy_dangling > 0 {
            h_details.push(format!("dangling parent: {}", info.hierarchy_dangling));
        }
        if info.hierarchy_stale_parent > 0 {
            h_details.push(format!("stale parent: {}", info.hierarchy_stale_parent));
        }
        if info.hierarchy_cycle > 0 {
            h_details.push(format!("cycle: {}", info.hierarchy_cycle));
        }
        checks.push(EnvironmentCheck {
            name: "Project hierarchy".to_string(),
            passed: true,
            message: format!("{} sub-project(s) with broken parent link", total_hierarchy_issues),
            suggestion: Some(
                "Run `engramdb projects prune` to clear broken links (promotes affected sub-projects back to roots)".to_string(),
            ),
            details: h_details,
            status: Some(CheckStatus::Warn),
        });
    } else {
        checks.push(EnvironmentCheck {
            name: "Project hierarchy".to_string(),
            passed: true,
            message: "healthy".to_string(),
            suggestion: None,
            details: vec![],
            status: None,
        });
    }

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
            // Advisory: a missing project `.mcp.json` just means the user
            // hasn't run `engramdb setup` for project-scoped MCP. The MCP
            // integration works fine via absolute paths / user-scoped config,
            // so this must not flip the exit code.
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: true,
                message: ".mcp.json not found".to_string(),
                suggestion: Some(
                    "Add engramdb to .mcp.json, or install the Claude Code plugin".to_string(),
                ),
                details: vec![],
                status: Some(CheckStatus::Warn),
            };
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            // Advisory: a malformed project `.mcp.json` is a setup issue, not
            // store corruption — the MCP integration works via absolute paths /
            // user-scoped config, so this must not flip the exit code.
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: true,
                message: format!("invalid JSON: {}", e),
                suggestion: Some("Fix the JSON syntax in .mcp.json".to_string()),
                details: vec![],
                status: Some(CheckStatus::Warn),
            };
        }
    };

    let servers = match json.get("mcpServers") {
        Some(s) => s,
        None => {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: true,
                message: "missing 'mcpServers' key".to_string(),
                suggestion: Some(
                    "Add an 'mcpServers' object containing an 'engramdb' entry".to_string(),
                ),
                details: vec![],
                status: Some(CheckStatus::Warn),
            };
        }
    };

    let engramdb = match servers.get("engramdb") {
        Some(e) => e,
        None => {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: true,
                message: "missing 'mcpServers.engramdb' key".to_string(),
                suggestion: Some("Add an 'engramdb' entry under 'mcpServers'".to_string()),
                details: vec![],
                status: Some(CheckStatus::Warn),
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
                    passed: true,
                    message: format!("command '{}' not found on disk or PATH", cmd),
                    suggestion: Some("Check the 'command' path in .mcp.json".to_string()),
                    details: vec![],
                    status: Some(CheckStatus::Warn),
                };
            }
        }
    } else {
        return EnvironmentCheck {
            name: "MCP server configuration".to_string(),
            passed: true,
            message: "missing or invalid 'command' field".to_string(),
            suggestion: Some(
                "Add a 'command' string to mcpServers.engramdb in .mcp.json".to_string(),
            ),
            details: vec![],
            status: Some(CheckStatus::Warn),
        };
    }

    // Check args field
    if let Some(args) = engramdb.get("args") {
        if !args.is_array() {
            return EnvironmentCheck {
                name: "MCP server configuration".to_string(),
                passed: true,
                message: "'args' field is not an array".to_string(),
                suggestion: Some("Set 'args' to an array of strings in .mcp.json".to_string()),
                details: vec![],
                status: Some(CheckStatus::Warn),
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

    // Advisory: an uncached model is downloaded on first use, so a cold cache
    // doesn't mean the store is broken — render it as a warning, not a failure
    // that flips the exit code.
    EnvironmentCheck {
        name: "Embedding model cache".to_string(),
        passed: true,
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
        status: if has_models {
            None
        } else {
            Some(CheckStatus::Warn)
        },
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

    /// Synthetic daemon check injected into `doctor_environment` in tests, so the
    /// `ops` test suite does not depend "upward" on `daemon` (the real probe is
    /// `daemon::doctor::check_daemon`, exercised from the CLI layer's tests).
    fn test_daemon_check() -> EnvironmentCheck {
        EnvironmentCheck {
            name: "Embedding daemon".to_string(),
            passed: true,
            message: "disabled in config (models load in-process per MCP)".to_string(),
            suggestion: None,
            details: vec![],
            status: Some(CheckStatus::Info),
        }
    }

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

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
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

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
        let section_names: Vec<&str> = result.sections.iter().map(|s| s.name.as_str()).collect();

        assert_eq!(
            section_names,
            vec!["Project", "Projects", "Global settings & models", "Stats"]
        );
    }

    /// Create a fake git clone with a fixed remote URL so two directories
    /// compute the same (remote-derived) project ID.
    fn make_clone(root: &std::path::Path, name: &str, remote: &str) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git").join("config"),
            format!(
                "[remote \"origin\"]\n\turl = https://github.com/example/{}.git\n",
                remote
            ),
        )
        .unwrap();
        dir
    }

    /// A second clone of the same remote shares the registered checkout's
    /// project ID; doctor must surface this as a "Checkout identity" warning
    /// naming the other checkout's path.
    #[tokio::test]
    async fn test_environment_warns_on_shared_checkout_identity() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "doctor-conflict");
        let b = make_clone(tmp.path(), "clone-b", "doctor-conflict");
        // doctor reads the global file registry (redirected per-process by
        // the test-isolation arm), so register through it.
        let registry = crate::storage::FileRegistry::global().unwrap();
        MemoryStore::init(&a, &registry).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();

        let result = doctor_environment(&b, Some(&store_b), test_daemon_check()).await;
        let check = result
            .all_checks()
            .into_iter()
            .find(|c| c.name == "Checkout identity")
            .expect("shared-ID situation must surface as a doctor check");

        assert!(check.passed, "warning check must not fail the run");
        assert_eq!(check.status, Some(CheckStatus::Warn));
        let a_canon = a.canonicalize().unwrap();
        assert!(
            check.message.contains("shared with another checkout")
                && check.message.contains(&a_canon.display().to_string()),
            "message must name the other checkout, got: {}",
            check.message
        );
    }

    /// The sole (registered) checkout must NOT get a checkout-identity check.
    #[tokio::test]
    async fn test_environment_no_checkout_identity_check_for_sole_clone() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "doctor-no-conflict");
        let registry = crate::storage::FileRegistry::global().unwrap();
        let store = MemoryStore::init(&a, &registry).await.unwrap();

        let result = doctor_environment(&a, Some(&store), test_daemon_check()).await;
        assert!(
            !result
                .all_checks()
                .iter()
                .any(|c| c.name == "Checkout identity"),
            "no conflict check expected for the registered owner"
        );
    }

    #[tokio::test]
    async fn test_environment_store_not_initialized() {
        let temp_dir = TempDir::new().unwrap();

        let result = doctor_environment(temp_dir.path(), None, test_daemon_check()).await;
        let store_check = result
            .all_checks()
            .into_iter()
            .find(|c| c.name == "Store initialized")
            .unwrap();
        assert!(!store_check.passed);
        assert_eq!(store_check.message, "not found");
        assert!(store_check.suggestion.as_ref().unwrap().contains("init"));
    }

    // ---- subscribed-group checks (multi-project memories) ----

    #[tokio::test]
    async fn test_check_subscribed_groups_empty_when_no_subscriptions() {
        // A project with no group subscriptions yields no group checks at all,
        // so the doctor omits the whole subsection.
        let temp_dir = TempDir::new().unwrap();
        let checks = check_subscribed_groups(temp_dir.path()).await;
        assert!(checks.is_empty());
    }

    #[tokio::test]
    async fn test_check_subscribed_groups_reports_readable_group() {
        use crate::storage::RegistryBackend;
        use crate::types::{Memory, MemoryType, Provenance};

        let temp_dir = TempDir::new().unwrap();
        // doctor reads the global file registry (redirected per-process by the
        // test-isolation arm), so register + subscribe through it.
        let registry = crate::storage::FileRegistry::global().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let pid = store.project_id.clone();

        let gid = registry.create_group("doctor-grp").await.unwrap();
        registry.subscribe(&pid, &gid).await.unwrap();
        let group = MemoryStore::open_group(&gid).await.unwrap();
        group
            .create(&Memory::new(
                MemoryType::Convention,
                "group convention",
                "shared content",
                Provenance::human(),
            ))
            .await
            .unwrap();
        drop(group);

        let checks = check_subscribed_groups(temp_dir.path()).await;
        assert_eq!(checks.len(), 1, "one subscribed group ⇒ one check");
        let c = &checks[0];
        assert_eq!(c.name, "Group 'doctor-grp'");
        // A freshly-written store isn't fingerprint-stamped (only reindex does
        // that), so the reachable-but-untracked path reports Info/Pass — never a
        // failure that would flip the exit code.
        assert!(
            c.passed,
            "a readable subscribed group must not fail the run"
        );
        assert!(
            matches!(c.status, Some(CheckStatus::Info) | Some(CheckStatus::Pass)),
            "expected Info/Pass, got {:?} ({})",
            c.status,
            c.message
        );
    }

    #[tokio::test]
    async fn test_check_subscribed_groups_reports_uncreated_group_as_empty() {
        use crate::storage::RegistryBackend;
        // Subscribed to a group whose store was never written (no store dir) →
        // the empty side of empty-vs-corrupt: a benign Info, not a warning.
        let temp_dir = TempDir::new().unwrap();
        let registry = crate::storage::FileRegistry::global().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let pid = store.project_id.clone();

        // Register the group in the roster and subscribe, but never open/write
        // its store, so no store dir exists on disk.
        let gid = crate::storage::paths::compute_group_id("never-written-grp");
        registry.create_group("never-written-grp").await.unwrap();
        registry.subscribe(&pid, &gid).await.unwrap();

        let checks = check_subscribed_groups(temp_dir.path()).await;
        assert_eq!(checks.len(), 1);
        let c = &checks[0];
        assert!(c.passed);
        assert_eq!(c.status, Some(CheckStatus::Info));
        assert!(
            c.message.contains("no memories yet"),
            "uncreated group must read as empty, got: {}",
            c.message
        );
    }

    #[tokio::test]
    async fn test_environment_store_initialized() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), None, test_daemon_check()).await;
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

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
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

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
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

        let result = doctor_environment(temp_dir.path(), None, test_daemon_check()).await;
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

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
        let expected = result.all_checks().iter().all(|c| c.passed);
        assert_eq!(result.all_passed, expected);
    }

    #[tokio::test]
    async fn test_environment_all_passed_false_on_failure() {
        let temp_dir = TempDir::new().unwrap();
        // No init — "Store initialized" will fail

        let result = doctor_environment(temp_dir.path(), None, test_daemon_check()).await;
        assert!(!result.all_passed);
    }

    #[tokio::test]
    async fn test_environment_serializes_to_json() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
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
        assert_eq!(result.name, "Embedding model cache");
        // Advisory: an uncached model is fetched on first use, so it warns
        // rather than failing (does not gate the exit code).
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
        assert_eq!(result.message, "not cached");
        assert!(result.suggestion.is_some());
    }

    // `validate_models` (added in #54) is otherwise only exercised end-to-end
    // by one happy-path CLI integration test, so its failure/skip branches were
    // uncovered. Force the embedding model unavailable (empty model cache +
    // offline) and assert the deterministic shape: all four model checks are
    // reported, and the embedding check fails rather than the whole call
    // erroring. This drives the `None`/`Err` embedding arm plus the
    // disabled-feature `skip` arms without needing any model staged.
    #[tokio::test]
    async fn test_validate_models_reports_all_sections_when_unavailable() {
        let empty_cache = TempDir::new().unwrap();
        std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", empty_cache.path());
        std::env::set_var("ENGRAMDB_OFFLINE", "1");

        let config = crate::types::EngramConfig::default();
        let checks = validate_models(&config).await;

        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Embedding model",
                "NLI model",
                "Reranker model",
                "Title model"
            ],
            "validate_models must report every model section"
        );

        let embedding = checks
            .iter()
            .find(|c| c.name == "Embedding model")
            .expect("embedding check present");
        assert!(
            !embedding.passed,
            "an unavailable embedding model must fail, got: {}",
            embedding.message
        );
        assert!(
            embedding.suggestion.is_some(),
            "a failed model check should carry a remediation hint"
        );

        // Disabled-by-default features are skipped (Info), not failed.
        let nli = checks.iter().find(|c| c.name == "NLI model").unwrap();
        assert!(nli.passed && nli.status == Some(CheckStatus::Info));
        let rerank = checks.iter().find(|c| c.name == "Reranker model").unwrap();
        assert!(rerank.passed && rerank.status == Some(CheckStatus::Info));
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
        // Advisory: a missing project `.mcp.json` is a setup hint, not a
        // failure — it warns and never gates the exit code.
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
        assert!(result.message.contains("not found"));
    }

    #[test]
    fn test_check_mcp_config_deep_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".mcp.json"), "not json {{{").unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        // Advisory: a malformed `.mcp.json` is a setup issue, not store
        // corruption — it warns rather than failing the exit code.
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
        assert!(result.message.contains("invalid JSON"));
    }

    #[test]
    fn test_check_mcp_config_deep_missing_servers_key() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".mcp.json"), r#"{"other": {}}"#).unwrap();

        let result = check_mcp_config_deep(temp_dir.path());
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
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
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
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
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
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
        // Advisory now: whether engramdb is on PATH or not, a malformed
        // `.mcp.json` warns (passed=true) rather than gating the exit code.
        // The diagnostic message still flags either the bad args or the
        // missing command.
        assert!(result.passed);
        assert_eq!(result.status, Some(CheckStatus::Warn));
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
        // When not configured, status should be Warn; when configured, None
        if result.message == "not configured" {
            assert_eq!(result.status, Some(CheckStatus::Warn));
        } else {
            assert_eq!(result.status, None);
        }
    }

    #[test]
    fn test_check_claude_plugin_warn_when_missing() {
        let result = check_claude_plugin();
        assert_eq!(result.name, "Claude Code plugin");
        assert!(result.passed);
        // When not found, status should be Warn; when found, None
        if result.message == "not found" {
            assert_eq!(result.status, Some(CheckStatus::Warn));
        } else {
            assert_eq!(result.status, None);
        }
    }

    #[tokio::test]
    async fn test_check_embedding_backend_defaults() {
        let temp_dir = TempDir::new().unwrap();
        let result = check_embedding_backend(temp_dir.path()).await;
        assert_eq!(result.name, "Embedding backend");
        assert!(result.passed);
        assert!(result.message.contains("auto"));
    }

    #[test]
    fn test_humanize_interval_picks_largest_unit() {
        assert_eq!(humanize_interval(Duration::from_secs(1)), "1 second");
        assert_eq!(humanize_interval(Duration::from_secs(45)), "45 seconds");
        assert_eq!(humanize_interval(Duration::from_secs(60)), "1 minute");
        assert_eq!(humanize_interval(Duration::from_secs(3600)), "1 hour");
        assert_eq!(
            humanize_interval(Duration::from_secs(6 * 60 * 60)),
            "6 hours"
        );
        assert_eq!(humanize_interval(Duration::from_secs(86_400)), "1 day");
        assert_eq!(humanize_interval(Duration::from_secs(3 * 86_400)), "3 days");
    }

    #[tokio::test]
    async fn test_check_maintenance_enabled_by_default() {
        // No config file → defaults: auto-maintenance enabled, 6h interval, and
        // (in an isolated test data dir) no marker yet → "never".
        let temp_dir = TempDir::new().unwrap();
        let check = check_maintenance(temp_dir.path()).await;
        assert_eq!(check.name, "Auto-maintenance");
        assert!(check.passed);
        assert_eq!(check.status, Some(CheckStatus::Info));
        assert!(
            check.message.starts_with("enabled (every "),
            "unexpected message: {}",
            check.message
        );
        assert!(
            check.details.iter().any(|d| d.contains("last run:")),
            "expected a last-run detail line, got {:?}",
            check.details
        );
    }

    #[tokio::test]
    async fn test_check_maintenance_disabled_collapses() {
        // A config that disables maintenance must render the terse "disabled"
        // form with no interval/last-run noise.
        let temp_dir = TempDir::new().unwrap();
        let cfg_dir = temp_dir.path().join(".engramdb");
        async_fs::create_dir_all(&cfg_dir).await.unwrap();
        async_fs::write(
            cfg_dir.join("config.toml"),
            "[maintenance]\nenabled = false\n",
        )
        .await
        .unwrap();

        let check = check_maintenance(temp_dir.path()).await;
        assert_eq!(check.name, "Auto-maintenance");
        assert!(check.passed);
        assert_eq!(check.message, "disabled");
        assert!(check.details.is_empty());
    }

    #[tokio::test]
    async fn test_build_registry_checks_returns_two_checks() {
        let temp_dir = TempDir::new().unwrap();
        let info = load_registry_info(temp_dir.path()).await;
        let checks = build_registry_checks(&info);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "Registered projects");
        assert_eq!(checks[1].name, "Project hierarchy");
    }

    #[tokio::test]
    async fn test_build_registry_checks_shows_reachable_count() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 5,
            reachable_projects: 3,
            orphan_dirs: 0,
            loaded: true,
            hierarchy_dangling: 0,
            hierarchy_stale_parent: 0,
            hierarchy_cycle: 0,
            conflicting_checkout: None,
        };
        let checks = build_registry_checks(&info);
        assert!(checks[0].message.contains("5 registered"));
        let details_str = checks[0].details.join(" ");
        assert!(details_str.contains("stale: 2"));
        assert!(checks[0].suggestion.as_ref().unwrap().contains("prune"));
        assert_eq!(checks[0].status, Some(CheckStatus::Warn));
    }

    #[tokio::test]
    async fn test_build_registry_checks_no_stale_no_hint() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 2,
            reachable_projects: 2,
            orphan_dirs: 0,
            loaded: true,
            hierarchy_dangling: 0,
            hierarchy_stale_parent: 0,
            hierarchy_cycle: 0,
            conflicting_checkout: None,
        };
        let checks = build_registry_checks(&info);
        let details_str = checks[0].details.join(" ");
        assert!(!details_str.contains("stale"));
        assert!(checks[0].suggestion.is_none());
        assert_eq!(checks[0].status, None);
    }

    #[tokio::test]
    async fn test_build_registry_checks_orphans_warn() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 2,
            reachable_projects: 2,
            orphan_dirs: 10,
            loaded: true,
            hierarchy_dangling: 0,
            hierarchy_stale_parent: 0,
            hierarchy_cycle: 0,
            conflicting_checkout: None,
        };
        let checks = build_registry_checks(&info);
        assert_eq!(checks[0].status, Some(CheckStatus::Warn));
        let details_str = checks[0].details.join(" ");
        assert!(details_str.contains("orphan data dirs: 10"));
        assert!(checks[0].suggestion.as_ref().unwrap().contains("prune"));
    }

    #[tokio::test]
    async fn test_build_registry_checks_hierarchy_healthy_passes_silently() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 2,
            reachable_projects: 2,
            orphan_dirs: 0,
            loaded: true,
            hierarchy_dangling: 0,
            hierarchy_stale_parent: 0,
            hierarchy_cycle: 0,
            conflicting_checkout: None,
        };
        let checks = build_registry_checks(&info);
        let hierarchy = checks
            .iter()
            .find(|c| c.name == "Project hierarchy")
            .unwrap();
        assert_eq!(hierarchy.status, None);
        assert!(hierarchy.message.contains("healthy"));
        assert!(hierarchy.suggestion.is_none());
    }

    #[tokio::test]
    async fn test_build_registry_checks_hierarchy_issues_warn() {
        let info = RegistryInfo {
            in_registry: true,
            total_projects: 3,
            reachable_projects: 3,
            orphan_dirs: 0,
            loaded: true,
            hierarchy_dangling: 1,
            hierarchy_stale_parent: 1,
            hierarchy_cycle: 2,
            conflicting_checkout: None,
        };
        let checks = build_registry_checks(&info);
        let hierarchy = checks
            .iter()
            .find(|c| c.name == "Project hierarchy")
            .unwrap();
        assert_eq!(hierarchy.status, Some(CheckStatus::Warn));
        assert!(hierarchy.message.contains("4"));
        let details_str = hierarchy.details.join(" ");
        assert!(details_str.contains("dangling parent: 1"));
        assert!(details_str.contains("stale parent: 1"));
        assert!(details_str.contains("cycle: 2"));
        assert!(hierarchy.suggestion.as_ref().unwrap().contains("prune"));
    }

    #[tokio::test]
    async fn test_project_section_skipped_when_not_initialized() {
        let temp_dir = TempDir::new().unwrap();

        let result = doctor_environment(temp_dir.path(), None, test_daemon_check()).await;
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
    async fn test_check_write_lock_held_warns() {
        use fs4::fs_std::FileExt;

        let project_id = "lock-test-project";
        let lock_dir = crate::storage::paths::global_data_dir()
            .unwrap()
            .join("projects")
            .join(project_id);
        std::fs::create_dir_all(&lock_dir).unwrap();
        let lock_path = lock_dir.join("write.lock");

        // Create and hold an exclusive lock
        let lock_file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        lock_file.lock_exclusive().unwrap();

        let result = check_write_lock(project_id).await;
        assert_eq!(result.name, "Write lock");
        assert_eq!(result.status, Some(CheckStatus::Warn));
        assert!(result.message.contains("held by active process"));

        // Clean up
        lock_file.unlock().unwrap();
        let _ = std::fs::remove_dir_all(&lock_dir);
    }

    #[tokio::test]
    async fn test_check_write_lock_not_held() {
        let project_id = "lock-test-not-held";
        let lock_dir = crate::storage::paths::global_data_dir()
            .unwrap()
            .join("projects")
            .join(project_id);
        std::fs::create_dir_all(&lock_dir).unwrap();
        let lock_path = lock_dir.join("write.lock");

        // Create lock file but don't hold a lock
        std::fs::File::create(&lock_path).unwrap();

        let result = check_write_lock(project_id).await;
        assert_eq!(result.name, "Write lock");
        assert!(result.passed);
        assert_eq!(result.status, None);
        assert!(result.message.contains("no active writer"));

        // Clean up
        let _ = std::fs::remove_dir_all(&lock_dir);
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

        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
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
    async fn test_environment_unset_up_shows_only_project_section() {
        let temp_dir = TempDir::new().unwrap();

        // With no .engramdb setup, the report collapses to a single Project
        // section carrying only the "not set up" notice — the global sections
        // (Projects / Global settings & models / Stats) are suppressed.
        let result = doctor_environment(temp_dir.path(), None, test_daemon_check()).await;
        let section_names: Vec<&str> = result.sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(section_names, vec!["Project"]);

        let names: Vec<&str> = result
            .all_checks()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["Store initialized"]);
        assert!(!result.all_passed);

        // None of the global checks should be computed when unset-up.
        assert!(!names.contains(&"Global disk usage"));
        assert!(!names.contains(&"Registered projects"));
        assert!(!names.contains(&"Manifest stats"));
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
    async fn test_global_section_has_models_and_embeddings_subsections() {
        let temp_dir = TempDir::new().unwrap();
        // The global sections only render for an initialized project.
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let result = doctor_environment(temp_dir.path(), Some(&store), test_daemon_check()).await;
        let global = result
            .sections
            .iter()
            .find(|s| s.name == "Global settings & models")
            .unwrap();
        let subsection_names: Vec<&str> =
            global.subsections.iter().map(|s| s.name.as_str()).collect();
        assert!(
            subsection_names.contains(&"Embeddings"),
            "missing Embeddings subsection under Global settings & models"
        );
        assert!(
            subsection_names.contains(&"Models"),
            "missing Models subsection under Global settings & models"
        );
        // The global gitignore moved up to a top-level Global check.
        assert!(
            global.checks.iter().any(|c| c.name == "Global gitignore"),
            "missing Global gitignore check under Global settings & models"
        );
    }

    #[tokio::test]
    async fn test_models_subsection_describes_embedding_model() {
        let temp_dir = TempDir::new().unwrap();
        let checks = check_active_models(temp_dir.path()).await;
        // Embedding model is always present and carries a "what it's for" detail.
        let embed = checks
            .iter()
            .find(|c| c.name == "Embedding model")
            .expect("embedding model must always be described");
        assert_eq!(embed.status, Some(CheckStatus::Info));
        assert!(!embed.details.is_empty(), "model needs a description");
        // Defaults: NLI and reranker are disabled, so no model is loaded for them.
        assert!(!checks.iter().any(|c| c.name == "NLI model"));
        assert!(!checks.iter().any(|c| c.name == "Reranker model"));
        // Default title strategy is T5.
        assert!(checks.iter().any(|c| c.name == "Title model"));
    }
}

// ===========================================================================
// Epistemic checks (§10.1–§10.3)
// ===========================================================================

/// Which epistemic check produced a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicFindingKind {
    /// §10.1 — files matching `valid_while.invalidated_by` changed since the
    /// memory was last verified. `--fix` flips to `NeedsReview`.
    InvalidatedPath,
    /// §10.2 — an observation unverified for longer than
    /// `[epistemic] observation_review_days`. Never flips status (age alone
    /// is not evidence of wrongness; decay already handles ranking).
    StaleObservation,
    /// §10.3 — a memory this one was derived from is missing, challenged, or
    /// under review (one level only, TMS-lite). `--fix` flips to
    /// `NeedsReview`.
    DerivedFromInvalid,
}

/// One finding from the epistemic doctor pass.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EpistemicFinding {
    pub id: String,
    pub summary: String,
    pub kind: EpistemicFindingKind,
    /// Human-readable explanation (what changed / how stale / which source).
    pub detail: String,
    /// Whether `--fix` flipped this memory to `NeedsReview` in this run.
    pub fixed: bool,
}

/// Enrichment gaps: live memories that carry a (type-derived or explicit)
/// epistemic class but none of the metadata that makes the class actionable.
/// Pre-epistemic stores start with EVERY memory in this state — the class
/// itself is materialized from the frozen type mapping, but premises, watch
/// globs, and verification stamps only accrue as memories are touched.
/// Report-only: gaps are an enrichment opportunity, never a defect.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct EnrichmentGaps {
    /// Decision-class memories with no recorded premise ("because C").
    pub decisions_without_premise: usize,
    /// Observation-class memories with no `invalidated_by` watch globs
    /// (the §10.1 doctor check can never fire for these).
    pub observations_without_watch: usize,
    /// Live memories examined.
    pub total_live: usize,
}

impl EnrichmentGaps {
    /// Count gaps over a set of live memories (shared by the doctor pass and
    /// the session-end prompt).
    pub fn count<'a, I: IntoIterator<Item = &'a crate::types::Memory>>(live: I) -> Self {
        use crate::types::Epistemic;
        let mut gaps = Self::default();
        for m in live {
            gaps.total_live += 1;
            match m.epistemic {
                Epistemic::Decision => {
                    if m.valid_while
                        .as_ref()
                        .and_then(|v| v.premise.as_ref())
                        .is_none()
                    {
                        gaps.decisions_without_premise += 1;
                    }
                }
                Epistemic::Observation => {
                    if m.valid_while
                        .as_ref()
                        .is_none_or(|v| v.invalidated_by.is_empty())
                    {
                        gaps.observations_without_watch += 1;
                    }
                }
                Epistemic::Fact => {}
            }
        }
        gaps
    }

    pub fn any(&self) -> bool {
        self.decisions_without_premise > 0 || self.observations_without_watch > 0
    }
}

/// Result of the epistemic doctor pass.
#[must_use]
#[derive(Debug, serde::Serialize)]
pub struct EpistemicDoctorResult {
    pub findings: Vec<EpistemicFinding>,
    /// Memories examined (live memories with the relevant fields).
    pub checked: usize,
    /// Enrichment gaps across the live set (report-only, see
    /// [`EnrichmentGaps`]).
    pub gaps: EnrichmentGaps,
}

/// Run the three epistemic checks (§10.1–§10.3) over every live memory.
///
/// Report-only by default (E4); with `fix = true` the §10.1/§10.3 findings
/// flip the memory to `NeedsReview` and record a challenge carrying the
/// machine-readable origin tag that `ops::verify` clears
/// ([`super::verify::DOCTOR_ORIGIN_INVALIDATED_PATH`] /
/// [`super::verify::DOCTOR_ORIGIN_DERIVED_FROM`]). Already-flagged memories
/// (same tag pending) are reported but not re-flipped, so repeated runs are
/// idempotent. Invalidated memories are skipped entirely — their windows are
/// closed; there is nothing left to review.
pub async fn doctor_epistemic(
    store: &MemoryStore,
    config: &crate::types::EngramConfig,
    fix: bool,
) -> Result<EpistemicDoctorResult> {
    use super::verify::{DOCTOR_ORIGIN_DERIVED_FROM, DOCTOR_ORIGIN_INVALIDATED_PATH};
    use crate::types::{Epistemic, Status};
    use chrono::{DateTime, Utc};

    let now = Utc::now();
    let ids = store.list_ids().await?;
    let loaded = store.get_batch(&ids).await?;
    let live: Vec<&crate::types::Memory> = loaded
        .iter()
        .map(|(_, m)| m)
        .filter(|m| !m.is_invalidated_at(now))
        .collect();

    // Status of every memory by id, for the derived-from check (§10.3) —
    // includes invalidated ones (an invalidated source also invalidates the
    // derivation, and a missing id is a finding too).
    let status_by_id: std::collections::HashMap<&str, (Status, bool)> = loaded
        .iter()
        .map(|(id, m)| (id.as_str(), (m.status, m.is_invalidated_at(now))))
        .collect();

    // §10.1 needs file mtimes. Walk the project tree ONCE, collecting
    // repo-relative paths + mtimes; each memory's globs are then matched in
    // memory. Skips dotted dirs (.git, .engramdb) and build output.
    let needs_walk = live.iter().any(|m| {
        m.valid_while
            .as_ref()
            .is_some_and(|v| !v.invalidated_by.is_empty())
    });
    let file_mtimes: Vec<(String, DateTime<Utc>)> = if needs_walk {
        collect_file_mtimes(&store.project_dir)
    } else {
        Vec::new()
    };

    let review_days = config.epistemic.observation_review_days;
    let mut findings = Vec::new();

    for memory in &live {
        let anchor = memory.verified_at.unwrap_or(memory.created_at);

        // §10.1 invalidated-path.
        if let Some(validity) = &memory.valid_while {
            if !validity.invalidated_by.is_empty() {
                let newest_match = file_mtimes
                    .iter()
                    .filter(|(path, _)| {
                        crate::scope::physical::matches(&validity.invalidated_by, path)
                    })
                    .map(|(_, mtime)| *mtime)
                    .max();
                if let Some(newest) = newest_match {
                    if newest > anchor {
                        let fixed = fix
                            && flip_to_needs_review(
                                store,
                                &memory.id,
                                DOCTOR_ORIGIN_INVALIDATED_PATH,
                                "invalidation paths changed since last verification",
                            )
                            .await;
                        findings.push(EpistemicFinding {
                            id: memory.id.clone(),
                            summary: memory.summary.clone(),
                            kind: EpistemicFindingKind::InvalidatedPath,
                            detail: format!(
                                "watched paths modified {} (last verified {})",
                                newest.format("%Y-%m-%d"),
                                anchor.format("%Y-%m-%d")
                            ),
                            fixed,
                        });
                    }
                }
            }
        }

        // §10.2 stale-observation (report-only, never flips).
        if memory.epistemic == Epistemic::Observation
            && review_days > 0
            && now - anchor > chrono::Duration::days(review_days as i64)
        {
            findings.push(EpistemicFinding {
                id: memory.id.clone(),
                summary: memory.summary.clone(),
                kind: EpistemicFindingKind::StaleObservation,
                detail: format!(
                    "observation unverified for {} days (review window {} days) — re-verify or delete",
                    (now - anchor).num_days(),
                    review_days
                ),
                fixed: false,
            });
        }

        // §10.3 derived-from cascade (one level, no transitive propagation).
        if let Some(validity) = &memory.valid_while {
            for source_id in &validity.derived_from {
                let problem = match status_by_id.get(source_id.as_str()) {
                    None => Some("missing".to_string()),
                    Some((_, true)) => Some("invalidated".to_string()),
                    Some((Status::Challenged, _)) => Some("challenged".to_string()),
                    Some((Status::NeedsReview, _)) => Some("under review".to_string()),
                    Some((Status::Active, _)) => None,
                };
                if let Some(problem) = problem {
                    let fixed = fix
                        && flip_to_needs_review(
                            store,
                            &memory.id,
                            DOCTOR_ORIGIN_DERIVED_FROM,
                            "a source this memory was derived from is invalid",
                        )
                        .await;
                    findings.push(EpistemicFinding {
                        id: memory.id.clone(),
                        summary: memory.summary.clone(),
                        kind: EpistemicFindingKind::DerivedFromInvalid,
                        detail: format!("derived from {source_id} which is {problem}"),
                        fixed,
                    });
                    break; // one finding per memory for this check
                }
            }
        }
    }

    Ok(EpistemicDoctorResult {
        checked: live.len(),
        gaps: EnrichmentGaps::count(live.iter().copied()),
        findings,
    })
}

/// Standalone enrichment-gap count (one batched load) for surfaces that
/// don't run the full doctor pass — the session-end prompt uses this.
pub async fn enrichment_gaps(store: &MemoryStore) -> Result<EnrichmentGaps> {
    let now = chrono::Utc::now();
    let ids = store.list_ids().await?;
    let loaded = store.get_batch(&ids).await?;
    Ok(EnrichmentGaps::count(
        loaded
            .iter()
            .map(|(_, m)| m)
            .filter(|m| !m.is_invalidated_at(now)),
    ))
}

/// Flip a memory to `NeedsReview` with a tagged doctor challenge (E4).
/// Idempotent: if a challenge with the same origin tag is already pending,
/// nothing is written. Returns whether a flip was actually performed.
async fn flip_to_needs_review(
    store: &MemoryStore,
    id: &str,
    origin_tag: &str,
    reason: &str,
) -> bool {
    // Pre-check outside the write path: an already-flagged memory must not be
    // rewritten (update_with always bumps updated_at) nor re-reported as
    // freshly fixed on repeated `doctor --fix` runs.
    match store.get(id).await {
        Ok(existing)
            if existing
                .challenges
                .iter()
                .any(|c| c.evidence.starts_with(origin_tag)) =>
        {
            return false;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(memory_id = %id, "doctor --fix pre-check failed: {e}");
            return false;
        }
    }
    let tag = origin_tag.to_string();
    let evidence = format!("{origin_tag} {reason}");
    let result = store
        .update_with(id, move |memory| {
            if memory
                .challenges
                .iter()
                .any(|c| c.evidence.starts_with(&tag))
            {
                return Ok(()); // already flagged; keep idempotent
            }
            memory
                .challenges
                .push(crate::types::Challenge::new(evidence.clone()));
            memory.status = crate::types::Status::NeedsReview;
            Ok(())
        })
        .await;
    match result {
        Ok(saved) => saved.status == crate::types::Status::NeedsReview,
        Err(e) => {
            tracing::warn!(memory_id = %id, "doctor --fix flip failed: {e}");
            false
        }
    }
}

/// Walk the project tree collecting `(repo-relative path, mtime)` pairs.
/// Skips dotted directories (`.git`, `.engramdb`, …) and `target`.
fn collect_file_mtimes(project_dir: &Path) -> Vec<(String, chrono::DateTime<chrono::Utc>)> {
    let mut out = Vec::new();
    let mut stack = vec![project_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(rel) = path.strip_prefix(project_dir) {
                        out.push((
                            rel.to_string_lossy().replace('\\', "/"),
                            chrono::DateTime::<chrono::Utc>::from(modified),
                        ));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod epistemic_doctor_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{EngramConfig, Epistemic, Memory, MemoryType, Provenance, Status, Validity};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    async fn setup() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    fn memory(id: &str, type_: MemoryType) -> Memory {
        let mut m = Memory::new(type_, id, "content", Provenance::human());
        m.id = id.to_string();
        m
    }

    /// Enrichment gaps count legacy-shaped memories (class present, metadata
    /// absent) and skip enriched + invalidated ones. Every pre-epistemic
    /// store starts with all-diagonal, all-unenriched memories — this is the
    /// signal doctor and the session-end prompt surface.
    #[tokio::test]
    async fn enrichment_gaps_count_legacy_shaped_memories() {
        let (_t, store) = setup().await;

        // Legacy-shaped decision: class decision (diagonal), no premise.
        store
            .create(&memory("gap-dec", MemoryType::Decision))
            .await
            .unwrap();
        // Enriched decision: premise recorded -> no gap.
        let mut enriched = memory("ok-dec", MemoryType::Decision);
        enriched.valid_while = Some(Validity {
            premise: Some("because C".into()),
            ..Default::default()
        });
        store.create(&enriched).await.unwrap();
        // Legacy-shaped observation: class observation (debug diagonal), no watch globs.
        store
            .create(&memory("gap-obs", MemoryType::Debug))
            .await
            .unwrap();
        // Watched observation -> no gap.
        let mut watched = memory("ok-obs", MemoryType::Debug);
        watched.valid_while = Some(Validity {
            invalidated_by: vec!["src/**".into()],
            ..Default::default()
        });
        store.create(&watched).await.unwrap();
        // Fact class never gaps; invalidated memories are excluded entirely.
        store
            .create(&memory("fact", MemoryType::Convention))
            .await
            .unwrap();
        let mut dead = memory("dead-dec", MemoryType::Decision);
        dead.invalidated_at = Some(Utc::now() - Duration::days(1));
        store.create(&dead).await.unwrap();

        let gaps = enrichment_gaps(&store).await.unwrap();
        assert_eq!(gaps.decisions_without_premise, 1, "{gaps:?}");
        assert_eq!(gaps.observations_without_watch, 1, "{gaps:?}");
        assert_eq!(gaps.total_live, 5, "{gaps:?}");
        assert!(gaps.any());

        // The full doctor pass carries the same counts.
        let result = doctor_epistemic(&store, &EngramConfig::default(), false)
            .await
            .unwrap();
        assert_eq!(result.gaps.decisions_without_premise, 1);
        assert_eq!(result.gaps.observations_without_watch, 1);
    }

    #[tokio::test]
    async fn invalidated_path_check_flags_changed_files() {
        let (tmp, store) = setup().await;
        // A watched file, modified NOW (i.e. after verified_at below).
        std::fs::write(tmp.path().join("watched.txt"), "v2").unwrap();

        let mut m = memory("doc-path", MemoryType::Hazard);
        m.valid_while = Some(Validity {
            invalidated_by: vec!["watched.txt".into()],
            ..Default::default()
        });
        m.verified_at = Some(Utc::now() - Duration::days(30));
        m.created_at = Utc::now() - Duration::days(60);
        store.create(&m).await.unwrap();

        // Report-only run: finding, no status change.
        let config = EngramConfig::default();
        let result = doctor_epistemic(&store, &config, false).await.unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(
            result.findings[0].kind,
            EpistemicFindingKind::InvalidatedPath
        );
        assert!(!result.findings[0].fixed);
        assert_eq!(store.get("doc-path").await.unwrap().status, Status::Active);

        // --fix flips to NeedsReview with the tagged challenge; a second
        // fix run is idempotent.
        let result = doctor_epistemic(&store, &config, true).await.unwrap();
        assert!(result.findings[0].fixed);
        let m = store.get("doc-path").await.unwrap();
        assert_eq!(m.status, Status::NeedsReview);
        assert_eq!(m.challenges.len(), 1);
        assert!(super::super::verify::is_doctor_review_finding(
            &m.challenges[0].evidence
        ));
        let _ = doctor_epistemic(&store, &config, true).await.unwrap();
        assert_eq!(store.get("doc-path").await.unwrap().challenges.len(), 1);

        // ops::verify clears the doctor review.
        let vr = super::super::verify::verify_memory(&store, "doc-path")
            .await
            .unwrap();
        assert!(vr.review_cleared);
        assert_eq!(store.get("doc-path").await.unwrap().status, Status::Active);
        // Verified now ⇒ the finding disappears on the next run.
        let result = doctor_epistemic(&store, &config, false).await.unwrap();
        assert!(result.findings.is_empty());
    }

    #[tokio::test]
    async fn stale_observation_reported_never_flipped() {
        let (_tmp, store) = setup().await;
        let mut m = memory("doc-obs", MemoryType::Debug); // Observation class
        m.epistemic = Epistemic::Observation;
        m.created_at = Utc::now() - Duration::days(120); // > 90d default
        store.create(&m).await.unwrap();

        let config = EngramConfig::default();
        let result = doctor_epistemic(&store, &config, true).await.unwrap();
        assert_eq!(result.findings.len(), 1);
        assert_eq!(
            result.findings[0].kind,
            EpistemicFindingKind::StaleObservation
        );
        assert!(!result.findings[0].fixed, "stale-observation never flips");
        assert_eq!(store.get("doc-obs").await.unwrap().status, Status::Active);

        // A recent verification silences it.
        store
            .update_with("doc-obs", |m| {
                m.verified_at = Some(Utc::now());
                Ok(())
            })
            .await
            .unwrap();
        let result = doctor_epistemic(&store, &config, false).await.unwrap();
        assert!(result.findings.is_empty());
    }

    #[tokio::test]
    async fn derived_from_cascade_flags_bad_sources() {
        let (_tmp, store) = setup().await;

        let mut source = memory("doc-src", MemoryType::Debug);
        source.status = Status::Challenged;
        store.create(&source).await.unwrap();

        let mut derived = memory("doc-derived", MemoryType::Context);
        derived.valid_while = Some(Validity {
            derived_from: vec!["doc-src".into()],
            ..Default::default()
        });
        store.create(&derived).await.unwrap();

        // A memory derived from a MISSING id is also flagged.
        let mut orphan = memory("doc-orphan", MemoryType::Context);
        orphan.valid_while = Some(Validity {
            derived_from: vec!["never-existed".into()],
            ..Default::default()
        });
        store.create(&orphan).await.unwrap();

        // A memory derived from a healthy source is NOT flagged.
        let healthy_src = memory("doc-good-src", MemoryType::Debug);
        store.create(&healthy_src).await.unwrap();
        let mut fine = memory("doc-fine", MemoryType::Context);
        fine.valid_while = Some(Validity {
            derived_from: vec!["doc-good-src".into()],
            ..Default::default()
        });
        store.create(&fine).await.unwrap();

        let config = EngramConfig::default();
        let result = doctor_epistemic(&store, &config, true).await.unwrap();
        let flagged: Vec<&str> = result
            .findings
            .iter()
            .filter(|f| f.kind == EpistemicFindingKind::DerivedFromInvalid)
            .map(|f| f.id.as_str())
            .collect();
        assert!(flagged.contains(&"doc-derived"));
        assert!(flagged.contains(&"doc-orphan"));
        assert!(!flagged.contains(&"doc-fine"));

        assert_eq!(
            store.get("doc-derived").await.unwrap().status,
            Status::NeedsReview
        );
        // One level only: nothing derives from doc-derived, and doc-fine
        // stays Active even though the STORE contains challenged memories.
        assert_eq!(store.get("doc-fine").await.unwrap().status, Status::Active);
    }

    #[tokio::test]
    async fn invalidated_memories_are_skipped() {
        let (_tmp, store) = setup().await;
        let mut m = memory("doc-dead", MemoryType::Debug);
        m.epistemic = Epistemic::Observation;
        m.created_at = Utc::now() - Duration::days(365);
        m.invalidated_at = Some(Utc::now() - Duration::days(1));
        store.create(&m).await.unwrap();

        let config = EngramConfig::default();
        let result = doctor_epistemic(&store, &config, false).await.unwrap();
        assert!(
            result.findings.is_empty(),
            "closed windows have nothing to review"
        );
    }
}
