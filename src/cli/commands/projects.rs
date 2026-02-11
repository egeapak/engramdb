//! Handler for the `engramdb projects` subcommand.

use crate::cli::app::ProjectsCommand;
use crate::cli::output::{
    AggregateStatsOutput, OutputFormatter, ProjectInfoOutput, ProjectListOutput,
};
use crate::ops::projects;
use crate::storage::RegistryBackend;
use anyhow::Result;
use std::path::Path;

/// Run the `projects` command with the given subcommand (defaults to `Info`).
pub async fn run_projects(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: Option<ProjectsCommand>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let command = command.unwrap_or(ProjectsCommand::Info);

    match command {
        ProjectsCommand::Info => {
            let info = projects::get_project_info(dir, registry).await?;
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
                    last_opened: e.last_opened,
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
                // Use inquire for confirmation
                let confirm = inquire::Confirm::new("Continue?")
                    .with_default(false)
                    .prompt()
                    .unwrap_or(false);
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
    }

    Ok(())
}
