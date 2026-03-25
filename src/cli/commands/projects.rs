//! Handler for the `engramdb projects` subcommand.

use crate::cli::app::ProjectsCommand;
use crate::cli::output::{
    AggregateStatsOutput, OutputFormatter, ProjectInfoOutput, ProjectListOutput,
};
use crate::cli::prompter::Prompter;
use crate::ops::projects;
use crate::storage::RegistryBackend;
use anyhow::Result;
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
                })
                .collect();
            formatter.print_project_list(&output);
        }
        ProjectsCommand::Delete { project_id, force } => {
            if !force {
                formatter.print_warning(&format!(
                    "This will remove project '{}' from the registry and delete its global data.",
                    project_id
                ));
                let confirm = prompter.confirm("Continue?", false).unwrap_or(false);
                if !confirm {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

            let result = projects::delete_project(registry, &project_id).await?;
            formatter.print_success(&format!(
                "Removed project from registry (path: {})",
                result.project_path
            ));
            if result.global_data_removed {
                formatter.print_success("Deleted global data (LanceDB + personal memories).");
            }
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

            if stale.is_empty() && orphan_count == 0 {
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
            orphan_pb.set_style(style);
            orphan_pb.set_prefix("orphan");
            if orphan_count == 0 {
                orphan_pb.finish_and_clear();
            }

            let result = projects::prune_stale_projects(registry, |phase| match phase {
                projects::PrunePhase::Stale => stale_pb.inc(1),
                projects::PrunePhase::Orphan => orphan_pb.inc(1),
            })
            .await?;
            stale_pb.finish_and_clear();
            orphan_pb.finish_and_clear();

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
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::prompter::MockPrompter;
    use crate::storage::registry::{InMemoryRegistry, Registry, RegistryEntry};

    #[tokio::test]
    async fn test_projects_delete_confirmed() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "test-proj".to_string(),
            project_path: temp_dir.path().to_string_lossy().to_string(),
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
}
