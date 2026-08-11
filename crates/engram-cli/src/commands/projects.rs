//! Handler for the `engramdb projects` subcommand.

use crate::app::ProjectsCommand;
use crate::output::{
    outln, AggregateStatsOutput, OutputFormatter, ProjectInfoOutput, ProjectListOutput,
};
use crate::progress;
use crate::prompter::Prompter;
use anyhow::{bail, Result};
use engramdb::ops::projects;
use engramdb::storage::RegistryBackend;
use engramdb::types::ProjectListGrouping;
use std::path::Path;

/// Render `(project_id, error)` pairs as objects.
///
/// A bare id would tell the user a directory could not be removed without
/// telling them anything they could act on; the errno text is the difference
/// between "it failed" and "it is owned by root".
fn failed_json(failed: &[(String, String)]) -> serde_json::Value {
    serde_json::Value::Array(
        failed
            .iter()
            .map(|(id, err)| serde_json::json!({ "project_id": id, "error": err }))
            .collect(),
    )
}

/// Report directories the sweep could not remove.
///
/// Deliberately a warning and deliberately not fatal: prune is a best-effort
/// sweep and the rest of its work stands, so the exit code says "the sweep
/// ran" while this says which directories it could not finish. Silence here
/// is what made `doctor`'s orphan warning look un-clearable — doctor counts
/// these as reclaimable, because in principle they are.
fn report_failed_reclaims(formatter: &OutputFormatter, failed: &[(String, String)]) {
    if failed.is_empty() {
        return;
    }
    formatter.print_warning(&format!(
        "Could not remove {} data director(ies); they are still on disk and \
         `doctor` will keep listing them until the cause is cleared:",
        failed.len()
    ));
    for (id, err) in failed {
        formatter.print_message(&format!("  {id}: {err}"));
    }
}

/// Run the `projects` command with the given subcommand (defaults to `Info`).
///
/// `ProjectsCommand::{Discover, Repair}` are NOT handled here: both rebuild an
/// index and so need model providers, and `lib.rs` dispatches them directly
/// alongside the other daemon-aware commands. Reaching them here means that
/// dispatch was removed.
pub async fn run_projects(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: Option<ProjectsCommand>,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
    grouping: ProjectListGrouping,
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
        ProjectsCommand::List { group } => {
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
            // The per-invocation `--group` flag overrides the config default.
            formatter.print_project_list(&output, group.unwrap_or(grouping));
        }
        ProjectsCommand::Discover { .. } | ProjectsCommand::Repair { .. } => {
            unreachable!("dispatched in lib.rs (both need model providers)")
        }
        ProjectsCommand::Delete {
            project_id,
            force,
            cascade,
            purge,
        } => {
            let json_mode = formatter.is_json();

            // JSON is machine-consumed: never prompt. Checked before anything
            // is inspected, so the contract is a property of the flags alone —
            // without this, `--format json projects delete <id>` aborted on a
            // pipe and blocked on an interactive prompt from a terminal.
            if !force && json_mode {
                anyhow::bail!(
                    "projects delete requires confirmation; re-run with --force in JSON mode"
                );
            }

            // Preview descendants so the confirmation prompt is informative.
            let reg = registry.load().await?;
            let descendants = engramdb::storage::collect_descendants(&reg, &project_id);
            drop(reg);

            if !descendants.is_empty() && !cascade {
                // Non-zero: nothing was deleted. Returning `Ok` here reported
                // success for work that was declined, so a `set -e` script saw
                // a clean delete and moved on.
                anyhow::bail!(
                    "project '{}' has {} sub-project(s): {}. Re-run with --cascade to delete them too, or unlink first.",
                    project_id,
                    descendants.len(),
                    descendants.join(", ")
                );
            }

            if !force {
                let data = if purge {
                    "delete their global data, personal memories included"
                } else {
                    "delete their index (personal memories are kept unless you pass --purge)"
                };
                if cascade && !descendants.is_empty() {
                    formatter.print_warning(&format!(
                        "This will remove project '{}' AND {} descendant(s) from the registry and {}.",
                        project_id,
                        descendants.len(),
                        data
                    ));
                } else {
                    formatter.print_warning(&format!(
                        "This will remove project '{}' from the registry and {}.",
                        project_id,
                        data.replace("their", "its")
                    ));
                }
                if purge {
                    formatter.print_warning(
                        "A project ID derived from a git remote is shared by every clone of that \
                         remote on this machine, and the registry records only one of them — so \
                         --purge can destroy another checkout's only copy of its personal \
                         memories.",
                    );
                }
                // Propagated, not swallowed. `unwrap_or(false)` turned a
                // prompt *failure* (no TTY, EOF on stdin) into a decline, and
                // a decline exits 0 — so `engramdb --format plain projects
                // delete <id>` from a script printed "Aborted." and reported
                // success for a project that is still registered. A refusal
                // the user never made must not read as a completed delete.
                if !prompter.confirm("Continue?", false)? {
                    // Nothing was deleted. Exiting 0 here would let `set -e`
                    // treat a declined delete as a done one.
                    formatter.print_message("Aborted.");
                    bail!("delete declined; nothing was removed");
                }
            }

            let result = projects::delete_project(registry, &project_id, cascade, purge).await?;

            if json_mode {
                // ONE document. Each `print_success` below is its own JSON
                // object, so the human path emitted two to four of them.
                outln!(
                    formatter,
                    "{}",
                    serde_json::json!({
                        "deleted": true,
                        "project_id": project_id,
                        "project_path": result.project_path,
                        "purge": purge,
                        "global_data_removed": result.global_data_removed,
                        "retained_irreplaceable": result.retained_irreplaceable,
                        "failed_to_reclaim": failed_json(&result.failed_to_reclaim),
                        "cascaded_ids": result.cascaded_ids,
                    })
                );
                return Ok(());
            }

            formatter.print_success(&format!(
                "Removed project from registry (path: {})",
                result.project_path
            ));
            if result.global_data_removed {
                if purge {
                    formatter.print_success(
                        "Deleted global data (LanceDB, personal memories, transcripts).",
                    );
                } else {
                    formatter.print_success("Deleted global data (LanceDB index).");
                }
            }
            if !result.retained_irreplaceable.is_empty() {
                formatter.print_message(&format!(
                    "Kept {} data director(ies) holding personal memories, archived \
                     transcripts or conversation summaries: {}. Re-run with --purge \
                     to delete them too.",
                    result.retained_irreplaceable.len(),
                    result.retained_irreplaceable.join(", ")
                ));
            }
            report_failed_reclaims(formatter, &result.failed_to_reclaim);
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
            let json_mode = formatter.is_json();

            // JSON is machine-consumed: never prompt. Checked before anything
            // is inspected so the contract is a property of the flags alone —
            // a script must not succeed or fail depending on whether this
            // machine happened to have something to prune.
            if !force && json_mode {
                anyhow::bail!("prune requires confirmation; re-run with --force in JSON mode");
            }

            // Preview what would be pruned
            let entries = projects::list_projects(registry).await?;
            let stale: Vec<_> = entries.iter().filter(|e| !e.exists).collect();
            let orphan_count = projects::count_orphan_dirs(registry).await?;
            let reg_snapshot = registry.load().await?;
            let hierarchy_issues = projects::scan_hierarchy_issues(&reg_snapshot);
            drop(reg_snapshot);

            if stale.is_empty() && orphan_count == 0 && hierarchy_issues.total() == 0 {
                if json_mode {
                    // Same object shape as a real prune so scripts parse one
                    // form — every key the success path emits, at its zero
                    // value.
                    outln!(
                        formatter,
                        "{}",
                        serde_json::json!({
                            "stale_removed": 0,
                            "stale_ids": [],
                            "orphans_removed": 0,
                            "orphan_ids": [],
                            "hierarchy_cleared": [],
                            "retained_irreplaceable": [],
                            "failed_to_reclaim": [],
                        })
                    );
                } else {
                    formatter.print_success("Nothing to prune.");
                }
                return Ok(());
            }

            // Preview uses print_message so --no-color and plain mode are
            // honored (the owo-colors styling this replaced ignored both), and
            // is suppressed entirely in JSON mode where stdout must carry
            // exactly one JSON object.
            if !json_mode {
                if stale.is_empty() {
                    formatter.print_message("  No stale registry entries found.");
                } else {
                    formatter.print_message(&format!(
                        "  Found {} stale registry entry(ies).",
                        stale.len()
                    ));
                }
                if orphan_count == 0 {
                    formatter.print_message("  No orphan data directories found.");
                } else {
                    formatter.print_message(&format!(
                        "  Found {} orphan data directory(ies) not in registry.",
                        orphan_count
                    ));
                }
                if hierarchy_issues.total() == 0 {
                    formatter.print_message("  No broken parent links found.");
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
                    formatter.print_message(&format!(
                        "  Found {} sub-project(s) with broken parent link ({}).",
                        hierarchy_issues.total(),
                        parts.join(", ")
                    ));
                }
            }

            if !force {
                // JSON mode already bailed at the top of this branch.
                // See the note on `delete` above: a failed prompt is not a
                // decline, and a decline is not a successful prune.
                let confirm = prompter.confirm("Remove all?", false)?;
                if !confirm {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

            // Progress bars are human-only chatter; hidden in JSON mode.
            // Construction lives in `crate::progress` so the draw target is a
            // parameter — that seam is the only way a test can see a rendered
            // bar (the default target is hidden under a pipe).
            let target = || progress::prune_draw_target(json_mode);
            let stale_pb = progress::make_bar(stale.len() as u64, "stale", target());
            let orphan_pb = progress::make_bar(orphan_count as u64, "orphan", target());
            let hierarchy_pb =
                progress::make_bar(hierarchy_issues.total() as u64, "links", target());

            let result = projects::prune_stale_projects(registry, |phase| match phase {
                projects::PrunePhase::Stale => stale_pb.inc(1),
                projects::PrunePhase::Orphan => orphan_pb.inc(1),
                projects::PrunePhase::Hierarchy => hierarchy_pb.inc(1),
            })
            .await?;
            stale_pb.finish_and_clear();
            orphan_pb.finish_and_clear();
            hierarchy_pb.finish_and_clear();

            if json_mode {
                outln!(
                    formatter,
                    "{}",
                    serde_json::json!({
                        "stale_removed": result.stale_removed,
                        "stale_ids": result.stale_ids,
                        "orphans_removed": result.orphans_removed,
                        "orphan_ids": result.orphan_ids,
                        "hierarchy_cleared": result.hierarchy_cleared,
                        "retained_irreplaceable": result.retained_irreplaceable,
                        "failed_to_reclaim": failed_json(&result.failed_to_reclaim),
                    })
                );
                return Ok(());
            }

            if result.stale_removed > 0 {
                formatter.print_success(&format!(
                    "Removed {} stale project(s) from registry.",
                    result.stale_removed
                ));
            }
            if result.orphans_removed > 0 {
                formatter.print_success(&format!(
                    "Removed {} orphan data directory(ies).",
                    result.orphans_removed
                ));
            }
            if !result.hierarchy_cleared.is_empty() {
                formatter.print_success(&format!(
                    "Cleared broken parent link on {} sub-project(s).",
                    result.hierarchy_cleared.len()
                ));
            }
            if !result.retained_irreplaceable.is_empty() {
                // Silence here would read as "everything was reclaimed", and
                // these directories hold the only copy of something — the user
                // needs to know they are still on disk.
                formatter.print_message(&format!(
                    "Kept {} data director(ies) holding personal memories, archived \
                     transcripts or conversation summaries: {}. Prune never deletes \
                     these; use `engramdb projects delete <id> --purge` if you mean to.",
                    result.retained_irreplaceable.len(),
                    result.retained_irreplaceable.join(", ")
                ));
            }
            report_failed_reclaims(formatter, &result.failed_to_reclaim);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompter::MockPrompter;
    use engramdb::storage::registry::{InMemoryRegistry, Registry, RegistryEntry};

    /// A formatter for the human/prompting path.
    ///
    /// NOT `OutputFormatter::new(None, false, ...)`: with no explicit format and
    /// no TTY — which is every test run — that resolves to **JSON**, and the
    /// delete/prune handlers refuse to prompt in JSON mode. These tests would
    /// then assert against the wrong refusal.
    fn human_formatter() -> OutputFormatter {
        OutputFormatter::new(Some(crate::app::OutputFormat::Plain), false, true)
    }

    /// `projects prune` must not destroy an unregistered project's personal
    /// memories — end to end, through the handler.
    ///
    /// This is the CLI half of a real data-loss bug: `doctor` warns "Registry:
    /// not registered", `--fix` answers it by running this very command, and
    /// the sweep used to delete the data directory because nothing in the
    /// registry pointed at it. `projects/<id>/personal/` is the only copy, so
    /// it went with it.
    ///
    /// What stops it now is `paths::holds_irreplaceable_data`, which retains
    /// any candidate still holding personal memories, transcript copies or a
    /// conversations table — broader than the earlier guard, which spared only
    /// the project the command happened to be invoked from. The invariant lives
    /// in `ops::projects`; what this pins is that the handler cannot lose it.
    #[tokio::test]
    async fn prune_keeps_an_unregistered_projects_personal_memories() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = engramdb::storage::MemoryStore::init(temp_dir.path(), &registry)
            .await
            .unwrap();
        let mut mem = engramdb::types::Memory::new(
            engramdb::types::MemoryType::Decision,
            "Personal note",
            "Only copy lives under projects/<id>/personal/",
            engramdb::types::Provenance::human(),
        );
        mem.visibility = engramdb::types::Visibility::Personal;
        store.create(&mem).await.unwrap();

        // On disk but absent from the registry — a lost registry.json, which
        // is exactly the state `doctor` offers this command as the fix for.
        registry.save(&Registry::default()).await.unwrap();

        run_projects(
            temp_dir.path(),
            &registry,
            Some(ProjectsCommand::Prune { force: true }),
            &human_formatter(),
            &MockPrompter::new(vec![]),
            ProjectListGrouping::default(),
        )
        .await
        .unwrap();

        assert!(
            engramdb::storage::MemoryStore::open(temp_dir.path())
                .await
                .unwrap()
                .get(&mem.id.to_string())
                .await
                .is_ok(),
            "prune destroyed an unregistered project's only copy of its personal memories"
        );
    }

    #[tokio::test]
    async fn test_projects_delete_confirmed() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "test-proj".to_string(),
            project_path: temp_dir.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = human_formatter();
        let prompter = MockPrompter::new(vec!["true"]);

        let result = run_projects(
            temp_dir.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "test-proj".to_string(),
                force: false,
                cascade: false,
                purge: false,
            }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
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
            subscriptions: vec![],
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = human_formatter();
        let prompter = MockPrompter::new(vec!["false"]);

        let result = run_projects(
            temp_dir.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "test-proj".to_string(),
                force: false,
                cascade: false,
                purge: false,
            }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await;

        // A decline is a REFUSAL, not a completed delete: exiting 0 here let a
        // `set -e` script read "Aborted." as success and carry on as though the
        // project were gone.
        let err = result.expect_err("a declined delete must not report success");
        assert!(
            err.to_string().contains("declined"),
            "the error must say the delete was declined, not look like a failure \
             to delete: {err}"
        );
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
            subscriptions: vec![],
        });
        data.projects.push(RegistryEntry {
            project_id: "child".to_string(),
            project_path: child_tmp.path().to_string_lossy().to_string(),
            parent_project_id: Some("parent".to_string()),
            subscriptions: vec![],
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = human_formatter();
        let prompter = MockPrompter::new(vec![]);

        let result = run_projects(
            parent_tmp.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "parent".to_string(),
                force: true, // doesn't matter — the block is informational
                cascade: false,
                purge: false,
            }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await;
        // Nothing was deleted, so this must NOT report success — returning
        // `Ok` here let a `set -e` script treat a declined delete as done.
        let err = result.expect_err("a refusal must exit non-zero");
        assert!(format!("{err}").contains("--cascade"));

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 2, "nothing should have been deleted");
    }

    /// `delete` never prompts in JSON mode either — and the refusal must come
    /// before anything is inspected, so the contract is a property of the flags
    /// alone rather than of what the registry happens to hold.
    #[tokio::test]
    async fn test_projects_delete_json_without_force_errors() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "test-proj".to_string(),
            project_path: temp_dir.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = OutputFormatter::new(None, true, true);
        // No scripted responses: JSON mode must error before ever prompting.
        let prompter = MockPrompter::new(vec![]);

        let err = run_projects(
            temp_dir.path(),
            &registry,
            Some(ProjectsCommand::Delete {
                project_id: "test-proj".to_string(),
                force: false,
                cascade: false,
                purge: false,
            }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .expect_err("JSON mode must refuse to prompt");
        assert!(format!("{err}").contains("--force"));

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1, "nothing should have been deleted");
    }

    /// The "requires --force in JSON mode" contract is a property of the flags
    /// alone. It used to be checked only on the path that had something to
    /// prune, so the same invocation succeeded or failed depending on the state
    /// of the machine it ran on — the one thing a scripted caller cannot test
    /// for in advance.
    #[tokio::test]
    async fn test_projects_prune_json_without_force_errors_even_with_nothing_to_prune() {
        let registry = InMemoryRegistry::new();
        let formatter = OutputFormatter::new(None, true, true);
        let prompter = MockPrompter::new(vec![]);

        let err = run_projects(
            Path::new("."),
            &registry,
            Some(ProjectsCommand::Prune { force: false }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .expect_err("JSON mode must refuse to prompt regardless of state");
        assert!(format!("{err}").contains("--force"));
    }

    #[tokio::test]
    async fn test_projects_prune_json_with_force_and_nothing_to_prune_is_ok() {
        let registry = InMemoryRegistry::new();
        let formatter = OutputFormatter::new(None, true, true);
        let prompter = MockPrompter::new(vec![]);

        let result = run_projects(
            Path::new("."),
            &registry,
            Some(ProjectsCommand::Prune { force: true }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_projects_prune_json_without_force_errors() {
        // A stale entry so there is something to prune.
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "gone".to_string(),
            project_path: "/nonexistent/prune-test-path".to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = OutputFormatter::new(None, true, true);
        // No scripted responses: JSON mode must error before ever prompting.
        let prompter = MockPrompter::new(vec![]);

        let result = run_projects(
            Path::new("."),
            &registry,
            Some(ProjectsCommand::Prune { force: false }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await;
        assert!(result.is_err(), "JSON mode without --force must error");
        // Nothing was pruned.
        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    #[tokio::test]
    async fn test_projects_prune_json_force_removes_stale() {
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "gone".to_string(),
            project_path: "/nonexistent/prune-test-path".to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        let registry = InMemoryRegistry::with(data);
        let formatter = OutputFormatter::new(None, true, true);
        let prompter = MockPrompter::new(vec![]);

        run_projects(
            Path::new("."),
            &registry,
            Some(ProjectsCommand::Prune { force: true }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .unwrap();

        let loaded = registry.load().await.unwrap();
        assert!(loaded.projects.is_empty(), "stale entry must be pruned");
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
            ProjectListGrouping::default(),
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
            ProjectListGrouping::default(),
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

    // =================================================================
    // Command-tier snapshots
    //
    // The tests above assert registry *state* — that declining leaves the
    // entry alone. These assert what the user was actually shown before they
    // answered: the warning naming what is about to be destroyed, the
    // confirmation and its default, and the outcome lines. Both flows here
    // are destructive and both default to "no", so the warning is the whole
    // safety story. See `crate::testutil` for why this tier exists.
    //
    // `projects prune` builds `indicatif` bars, but their default draw target
    // is stderr and `is_term()` is false under the runner, so they are hidden
    // and never reach the capture (which is the formatter's sink anyway).
    // What the bars *render* is covered separately in `crate::progress`,
    // which takes the draw target as a parameter and points it at an
    // `InMemoryTerm`.
    // =================================================================

    use crate::testutil::{
        capturing_json, capturing_plain, interaction, snap_command, TempProject,
    };

    /// The registry stores the *canonicalized* project path (`update` calls
    /// `Path::canonicalize`), and `delete` echoes it back. `TempProject::path`
    /// is the uncanonicalized handle — identical on Linux, but `/tmp` is a
    /// symlink on macOS — so normalization has to be given the resolved form
    /// or the temp path would leak into the snapshot.
    fn canonical_dir(p: &TempProject) -> std::path::PathBuf {
        p.path().canonicalize().unwrap_or_else(|_| p.path().into())
    }

    /// Register a stale entry: a path that does not exist, so
    /// `registry_entry_alive` classifies it as stale and prune has something
    /// to preview. Nothing is created under the global data dir, so the
    /// orphan and broken-parent-link counts stay at zero.
    async fn with_stale_entry(p: &TempProject) {
        p.registry
            .update(&p.path().join("moved-away"), "stale-project")
            .await
            .unwrap();
    }

    /// Accepting the confirmation. The project was really initialised, so the
    /// global-data line is part of the outcome too.
    #[tokio::test]
    async fn snap_projects_delete_confirmed() {
        let p = TempProject::new();
        let project_id = p.init_store().await.project_id.clone();

        let prompter = MockPrompter::new(vec!["yes"]);
        let (formatter, cap) = capturing_plain();
        run_projects(
            p.path(),
            &p.registry,
            Some(ProjectsCommand::Delete {
                project_id,
                force: false,
                cascade: false,
                purge: false,
            }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .unwrap();

        snap_command(
            "projects_delete_confirmed",
            &canonical_dir(&p),
            interaction(&prompter, &cap),
        );
    }

    /// Declining. The prompt defaults to `no`, so this is what a bare Enter
    /// does too — worth pinning for a command that deletes a project's index.
    ///
    /// A decline is an **error**, not a quiet success: it used to return `Ok`,
    /// so `set -e` scripts read "Aborted." as a completed delete and moved on.
    /// The returned message is part of that contract, so it goes in the
    /// snapshot rather than being asserted away with `is_err()`.
    #[tokio::test]
    async fn snap_projects_delete_declined() {
        let p = TempProject::new();
        let project_id = p.init_store().await.project_id.clone();

        let prompter = MockPrompter::new(vec!["no"]);
        let (formatter, cap) = capturing_plain();
        let err = run_projects(
            p.path(),
            &p.registry,
            Some(ProjectsCommand::Delete {
                project_id,
                force: false,
                cascade: false,
                purge: false,
            }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .expect_err("declining must not report success");

        // Still registered — the decline has to have been total.
        assert_eq!(p.registry.load().await.unwrap().projects.len(), 1);

        snap_command(
            "projects_delete_declined",
            &canonical_dir(&p),
            format!("{}--- error ---\n{err}\n", interaction(&prompter, &cap)),
        );
    }

    /// Accepting the confirmation. The three preview lines are the point:
    /// they are the only place the user learns *what* "Remove all?" covers,
    /// and each category reports even when it found nothing.
    #[tokio::test]
    async fn snap_projects_prune_confirmed() {
        let p = TempProject::new();
        with_stale_entry(&p).await;

        let prompter = MockPrompter::new(vec!["yes"]);
        let (formatter, cap) = capturing_plain();
        run_projects(
            p.path(),
            &p.registry,
            Some(ProjectsCommand::Prune { force: false }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .unwrap();

        snap_command(
            "projects_prune_confirmed",
            &canonical_dir(&p),
            interaction(&prompter, &cap),
        );
    }

    /// Declining after the same preview.
    #[tokio::test]
    async fn snap_projects_prune_declined() {
        let p = TempProject::new();
        with_stale_entry(&p).await;

        let prompter = MockPrompter::new(vec!["no"]);
        let (formatter, cap) = capturing_plain();
        run_projects(
            p.path(),
            &p.registry,
            Some(ProjectsCommand::Prune { force: false }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .unwrap();

        snap_command(
            "projects_prune_declined",
            &canonical_dir(&p),
            interaction(&prompter, &cap),
        );
    }

    /// JSON is machine-consumed, so prune refuses to prompt and bails. The
    /// preview is suppressed as well — stdout must carry exactly one JSON
    /// document or nothing — which leaves the error message as the only thing
    /// the caller gets. Empty prompts *and* empty streams are the assertion.
    #[tokio::test]
    async fn snap_projects_prune_json_refuses() {
        let p = TempProject::new();
        with_stale_entry(&p).await;

        let prompter = MockPrompter::new(vec![]);
        let (formatter, cap) = capturing_json();
        let err = run_projects(
            p.path(),
            &p.registry,
            Some(ProjectsCommand::Prune { force: false }),
            &formatter,
            &prompter,
            ProjectListGrouping::default(),
        )
        .await
        .expect_err("JSON mode must refuse to prompt");

        snap_command(
            "projects_prune_json_refuses",
            &canonical_dir(&p),
            format!("{}--- error ---\n{err}\n", interaction(&prompter, &cap)),
        );
    }
}
