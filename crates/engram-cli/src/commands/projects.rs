//! Handler for the `engramdb projects` subcommand.

use crate::app::ProjectsCommand;
use crate::output::{AggregateStatsOutput, OutputFormatter, ProjectInfoOutput, ProjectListOutput};
use crate::prompter::Prompter;
use anyhow::Result;
use engramdb::ops::projects;
use engramdb::storage::RegistryBackend;
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use std::path::Path;

/// Run the `projects` command with the given subcommand (defaults to `Info`).
pub async fn run_projects(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: Option<ProjectsCommand>,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    let command = command.unwrap_or(ProjectsCommand::Info);

    match command {
        ProjectsCommand::Info => {
            let info = projects::get_project_info(dir).await?;
            formatter.print_project_info(&ProjectInfoOutput {
                project_id: info.project_id,
                project_name: info.project_name,
                project_path: info.project_path,
                memory_count: info.memory_count,
                logical_scopes: info.logical_scopes,
                created_at: info.created_at,
                parent_project_id: info.parent_project_id,
            });
        }
        ProjectsCommand::List => {
            let entries = projects::list_projects(registry).await?;
            let output: Vec<ProjectListOutput> = entries
                .into_iter()
                .map(|e| ProjectListOutput {
                    project_id: e.project_id,
                    project_path: e.project_path,
                    exists: e.exists,
                    parent_project_id: e.parent_project_id,
                })
                .collect();
            formatter.print_project_list(&output);
        }
        ProjectsCommand::Delete {
            project_id,
            force,
            cascade,
        } => {
            // Preview descendants so the confirmation prompt is informative.
            let reg = registry.load().await?;
            let descendants = engramdb::storage::collect_descendants(&reg, &project_id);
            drop(reg);

            if !descendants.is_empty() && !cascade {
                formatter.print_warning(&format!(
                    "Project '{}' has {} sub-project(s): {}. Re-run with --cascade to delete them too, or unlink first.",
                    project_id,
                    descendants.len(),
                    descendants.join(", ")
                ));
                return Ok(());
            }

            if !force {
                if cascade && !descendants.is_empty() {
                    formatter.print_warning(&format!(
                        "This will remove project '{}' AND {} descendant(s) from the registry and delete their global data.",
                        project_id,
                        descendants.len()
                    ));
                } else {
                    formatter.print_warning(&format!(
                        "This will remove project '{}' from the registry and delete its global data.",
                        project_id
                    ));
                }
                let confirm = prompter.confirm("Continue?", false).unwrap_or(false);
                if !confirm {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

            let result = projects::delete_project(registry, &project_id, cascade).await?;
            formatter.print_success(&format!(
                "Removed project from registry (path: {})",
                result.project_path
            ));
            if result.global_data_removed {
                formatter.print_success("Deleted global data (LanceDB + personal memories).");
            }
            if !result.cascaded_ids.is_empty() {
                formatter.print_success(&format!(
                    "Cascade-deleted {} descendant project(s): {}",
                    result.cascaded_ids.len(),
                    result.cascaded_ids.join(", ")
                ));
            }
        }
        ProjectsCommand::Link { child, parent } => {
            projects::link_project(registry, &child, &parent).await?;
            formatter.print_success(&format!(
                "Linked project '{}' as sub-project of '{}'.",
                child, parent
            ));
        }
        ProjectsCommand::Unlink { project_id } => {
            projects::unlink_project(registry, &project_id).await?;
            formatter.print_success(&format!(
                "Unlinked project '{}' (now a root project).",
                project_id
            ));
        }
        ProjectsCommand::Stats => {
            let stats = projects::aggregate_stats(registry).await?;
            formatter.print_aggregate_stats(&AggregateStatsOutput {
                total_projects: stats.total_projects,
                reachable_projects: stats.reachable_projects,
                total_memories: stats.total_memories,
                by_type: stats.by_type,
            });
        }
        ProjectsCommand::Prune { force } => {
            // Preview what would be pruned
            let entries = projects::list_projects(registry).await?;
            let stale: Vec<_> = entries.iter().filter(|e| !e.exists).collect();
            let orphan_count = projects::count_orphan_dirs(registry).await?;
            let reg_snapshot = registry.load().await?;
            let hierarchy_issues = projects::scan_hierarchy_issues(&reg_snapshot);
            drop(reg_snapshot);

            if stale.is_empty() && orphan_count == 0 && hierarchy_issues.total() == 0 {
                formatter.print_success("Nothing to prune.");
                return Ok(());
            }

            if stale.is_empty() {
                println!("  {} stale registry entries found.", "No".green());
            } else {
                println!(
                    "  Found {} stale registry entry(ies).",
                    stale.len().yellow()
                );
            }
            if orphan_count == 0 {
                println!("  {} orphan data directories found.", "No".green());
            } else {
                println!(
                    "  Found {} orphan data directory(ies) not in registry.",
                    orphan_count.yellow()
                );
            }
            if hierarchy_issues.total() == 0 {
                println!("  {} broken parent links found.", "No".green());
            } else {
                let mut parts = Vec::new();
                if !hierarchy_issues.dangling.is_empty() {
                    parts.push(format!("{} dangling", hierarchy_issues.dangling.len()));
                }
                if !hierarchy_issues.stale_parent.is_empty() {
                    parts.push(format!(
                        "{} stale-parent",
                        hierarchy_issues.stale_parent.len()
                    ));
                }
                if !hierarchy_issues.cycle_members.is_empty() {
                    parts.push(format!("{} in cycle", hierarchy_issues.cycle_members.len()));
                }
                println!(
                    "  Found {} sub-project(s) with broken parent link ({}).",
                    hierarchy_issues.total().yellow(),
                    parts.join(", ")
                );
            }

            if !force {
                let confirm = prompter.confirm("Remove all?", false).unwrap_or(false);
                if !confirm {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

            let style = ProgressStyle::default_bar()
                .template("{prefix} [{bar:40.green/dim}] {pos}/{len} ({eta})")
                .unwrap()
                .progress_chars("=>-");

            let stale_pb = ProgressBar::new(stale.len() as u64);
            stale_pb.set_style(style.clone());
            stale_pb.set_prefix("stale");
            if stale.is_empty() {
                stale_pb.finish_and_clear();
            }

            let orphan_pb = ProgressBar::new(orphan_count as u64);
            orphan_pb.set_style(style.clone());
            orphan_pb.set_prefix("orphan");
            if orphan_count == 0 {
                orphan_pb.finish_and_clear();
            }

            let hierarchy_pb = ProgressBar::new(hierarchy_issues.total() as u64);
            hierarchy_pb.set_style(style);
            hierarchy_pb.set_prefix("links");
            if hierarchy_issues.total() == 0 {
                hierarchy_pb.finish_and_clear();
            }

            let result = projects::prune_stale_projects(registry, |phase| match phase {
                projects::PrunePhase::Stale => stale_pb.inc(1),
                projects::PrunePhase::Orphan => orphan_pb.inc(1),
                projects::PrunePhase::Hierarchy => hierarchy_pb.inc(1),
            })
            .await?;
            stale_pb.finish_and_clear();
            orphan_pb.finish_and_clear();
            hierarchy_pb.finish_and_clear();

            if result.stale_removed > 0 {
                println!(
                    "  {} Removed {} stale project(s) from registry.",
                    "✓".green(),
                    result.stale_removed.green()
                );
            }
            if result.orphans_removed > 0 {
                println!(
                    "  {} Removed {} orphan data directory(ies).",
                    "✓".green(),
                    result.orphans_removed.green()
                );
            }
            if !result.hierarchy_cleared.is_empty() {
                println!(
                    "  {} Cleared broken parent link on {} sub-project(s).",
                    "✓".green(),
                    result.hierarchy_cleared.len().green()
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompter::MockPrompter;
    use engramdb::storage::registry::{InMemoryRegistry, Registry, RegistryEntry};

    #[tokio::test]
    async fn test_projects_delete_confirmed() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "test-proj".to_string(),
            project_path: temp_dir.path().to_string_lossy().to_string(),
            parent_project_id: None,
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = OutputFormatter::new(None, false, true);
        let prompter = MockPrompter::new(vec!["true"]);

        let result = run_projects(
            temp_dir.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "test-proj".to_string(),
                force: false,
                cascade: false,
            }),
            &formatter,
            &prompter,
        )
        .await;

        assert!(result.is_ok());
        // Verify project was removed from registry
        let loaded = registry.load().await.unwrap();
        assert!(loaded.projects.is_empty());
    }

    #[tokio::test]
    async fn test_projects_delete_cancelled() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "test-proj".to_string(),
            project_path: temp_dir.path().to_string_lossy().to_string(),
            parent_project_id: None,
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = OutputFormatter::new(None, false, true);
        let prompter = MockPrompter::new(vec!["false"]);

        let result = run_projects(
            temp_dir.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "test-proj".to_string(),
                force: false,
                cascade: false,
            }),
            &formatter,
            &prompter,
        )
        .await;

        assert!(result.is_ok());
        // Verify project is still in registry (not deleted)
        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    #[tokio::test]
    async fn test_projects_delete_blocked_by_children_without_cascade() {
        let parent_tmp = tempfile::TempDir::new().unwrap();
        let child_tmp = tempfile::TempDir::new().unwrap();
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "parent".to_string(),
            project_path: parent_tmp.path().to_string_lossy().to_string(),
            parent_project_id: None,
        });
        data.projects.push(RegistryEntry {
            project_id: "child".to_string(),
            project_path: child_tmp.path().to_string_lossy().to_string(),
            parent_project_id: Some("parent".to_string()),
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = OutputFormatter::new(None, false, true);
        let prompter = MockPrompter::new(vec![]);

        let result = run_projects(
            parent_tmp.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "parent".to_string(),
                force: true, // doesn't matter — the block is informational
                cascade: false,
            }),
            &formatter,
            &prompter,
        )
        .await;
        assert!(result.is_ok(), "CLI returns Ok and prints a warning");

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 2, "nothing should have been deleted");
    }

    #[tokio::test]
    async fn test_projects_link_and_unlink_roundtrip() {
        use engramdb::storage::MemoryStore;
        let parent_tmp = tempfile::TempDir::new().unwrap();
        let child_tmp = tempfile::TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let parent_store = MemoryStore::init(parent_tmp.path(), &registry)
            .await
            .unwrap();
        let child_store = MemoryStore::init(child_tmp.path(), &registry)
            .await
            .unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let prompter = MockPrompter::new(vec![]);

        // Link.
        run_projects(
            parent_tmp.path(),
            &registry,
            Some(ProjectsCommand::Link {
                child: child_store.project_id.clone(),
                parent: parent_store.project_id.clone(),
            }),
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        let loaded = registry.load().await.unwrap();
        let child_entry = loaded
            .projects
            .iter()
            .find(|e| e.project_id == child_store.project_id)
            .unwrap();
        assert_eq!(
            child_entry.parent_project_id.as_deref(),
            Some(parent_store.project_id.as_str())
        );

        // Unlink.
        run_projects(
            parent_tmp.path(),
            &registry,
            Some(ProjectsCommand::Unlink {
                project_id: child_store.project_id.clone(),
            }),
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        let loaded = registry.load().await.unwrap();
        let child_entry = loaded
            .projects
            .iter()
            .find(|e| e.project_id == child_store.project_id)
            .unwrap();
        assert_eq!(child_entry.parent_project_id, None);
    }
}
