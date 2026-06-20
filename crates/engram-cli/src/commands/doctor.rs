//! Doctor (health check) command.

use crate::app::{DoctorCommand, ProjectsCommand};
use crate::output::{short_id, OutputFormatter};
use crate::prompter::Prompter;
use anyhow::Result;
use engramdb::ops::{
    doctor, doctor_environment, validate_models, CheckStatus, DoctorSection, EnvironmentDoctorResult,
};
use engramdb::storage::MemoryStore;
use std::io::IsTerminal;
use std::path::Path;

/// Run doctor with optional subcommand dispatch.
///
/// - `None` → full environment diagnostics (with optional `--fix`)
/// - `Some(DoctorCommand::Store)` → fast store-only health check
/// - `Some(DoctorCommand::Validate)` → load each model and test-infer
#[allow(clippy::too_many_arguments)]
pub async fn run_doctor(
    dir: &Path,
    global: bool,
    command: Option<DoctorCommand>,
    fix: bool,
    yes: bool,
    prompter: &dyn Prompter,
    formatter: &OutputFormatter,
) -> Result<()> {
    match command {
        Some(DoctorCommand::Store) => run_store_check(dir, global, formatter).await,
        Some(DoctorCommand::Validate) => run_validate(dir, global, formatter).await,
        None => run_environment_check(dir, global, fix, yes, prompter, formatter).await,
    }
}

/// Fast store-only health check (what MCP calls on session start).
async fn run_store_check(dir: &Path, global: bool, formatter: &OutputFormatter) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    let result = doctor(&store).await?;

    if result.healthy {
        formatter.print_success(&format!(
            "Store is healthy. {} memories indexed, {} on disk.",
            result.indexed, result.on_disk
        ));
    } else {
        if !result.stale_entries.is_empty() {
            formatter.print_warning(&format!(
                "{} stale index entries (in index but missing from disk):",
                result.stale_entries.len()
            ));
            for id in &result.stale_entries {
                println!("  {}", short_id(id));
            }
        }
        if !result.orphaned_files.is_empty() {
            formatter.print_warning(&format!(
                "{} orphaned files (on disk but not in index):",
                result.orphaned_files.len()
            ));
            for id in &result.orphaned_files {
                println!("  {}", short_id(id));
            }
        }
        formatter.print_message("\nRun `engramdb reindex` to repair.");
        // Unhealthy must exit non-zero so scripts/CI can gate on `doctor`.
        // The findings were already printed above; the error just sets the
        // exit code (main maps Err → exit 1).
        anyhow::bail!(
            "store is unhealthy ({} stale, {} orphaned)",
            result.stale_entries.len(),
            result.orphaned_files.len()
        );
    }

    Ok(())
}

/// Full environment diagnostics with actionable suggestions.
async fn run_environment_check(
    dir: &Path,
    global: bool,
    fix: bool,
    yes: bool,
    prompter: &dyn Prompter,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await.ok()
    } else {
        MemoryStore::open(dir).await.ok()
    };
    let check_dir = store
        .as_ref()
        .map(|s| s.project_dir.clone())
        .unwrap_or_else(|| dir.to_path_buf());
    let daemon_check = engramdb::daemon::check_daemon(&check_dir).await;
    let result = doctor_environment(&check_dir, store.as_ref(), daemon_check).await;
    formatter.print_environment_doctor(&result);

    // `--fix` takes over from here: it offers to repair the fixable issues
    // instead of just exiting non-zero, so we don't `bail!` in that mode.
    if fix {
        return apply_fixes(&check_dir, global, &result, yes, prompter, formatter).await;
    }

    // `all_passed` only reflects hard failures (`passed == false`): checks
    // rendered as Warn/Info carry `passed == true`, so warnings never flip
    // the exit code — only real failures exit non-zero.
    if !result.all_passed {
        anyhow::bail!("environment check found failing checks");
    }
    Ok(())
}

/// Load each downloaded/enabled model and run a tiny inference to confirm it
/// works, rendering the results through the shared environment-doctor printer.
async fn run_validate(dir: &Path, global: bool, formatter: &OutputFormatter) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await.ok()
    } else {
        MemoryStore::open(dir).await.ok()
    };
    let config_dir = store
        .as_ref()
        .map(|s| s.project_dir.clone())
        .unwrap_or_else(|| dir.to_path_buf());
    let config =
        engramdb::storage::config::load_config(&config_dir.join(".engramdb").join("config.toml"))
            .await
            .unwrap_or_default();

    let checks = validate_models(&config).await;
    let all_passed = checks.iter().all(|c| c.passed);
    let result = EnvironmentDoctorResult {
        sections: vec![DoctorSection {
            name: "Model validation".to_string(),
            checks,
            subsections: vec![],
        }],
        all_passed,
        store_check: None,
    };
    formatter.print_environment_doctor(&result);
    if !all_passed {
        anyhow::bail!("model validation found failing models");
    }
    Ok(())
}

/// A repair `--fix` can offer for a detected issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FixAction {
    /// Initialize EngramDB in this project.
    Init,
    /// Rebuild the LanceDB index (and re-embed unless `embeddings_only`).
    Reindex { embeddings_only: bool },
    /// Download the embedding model into the shared cache.
    DownloadEmbedding,
    /// Prune stale/orphaned projects from the registry.
    PruneProjects,
}

impl FixAction {
    /// The yes/no prompt shown for this fix.
    fn prompt(&self) -> &'static str {
        match self {
            FixAction::Init => "Initialize EngramDB in this project (engramdb init)?",
            FixAction::Reindex {
                embeddings_only: false,
            } => "Rebuild the index and re-embed memories (engramdb reindex)?",
            FixAction::Reindex {
                embeddings_only: true,
            } => "Re-embed memories with the current model (engramdb reindex --embeddings-only)?",
            FixAction::DownloadEmbedding => "Download the embedding model now?",
            FixAction::PruneProjects => {
                "Prune stale/orphaned projects from the registry (engramdb projects prune)?"
            }
        }
    }

    /// Apply the fix by delegating to the existing command/op.
    async fn apply(&self, dir: &Path, global: bool, formatter: &OutputFormatter) -> Result<()> {
        match self {
            FixAction::Init => {
                let registry = engramdb::storage::FileRegistry::global()?;
                crate::commands::run_init(dir, &registry, false, None, None, formatter).await
            }
            FixAction::Reindex { embeddings_only } => {
                crate::commands::run_reindex(dir, global, *embeddings_only, false, None, formatter)
                    .await
            }
            FixAction::DownloadEmbedding => {
                let config = engramdb::storage::config::load_config(
                    &dir.join(".engramdb").join("config.toml"),
                )
                .await
                .unwrap_or_default();
                // Resolving providers loads (and downloads, if missing) the
                // embedding model into the shared cache.
                let providers = engramdb::ops::resolve_engine_providers(&config, None, 1);
                if providers.embedding.is_some() {
                    formatter.print_success("Embedding model is ready.");
                    Ok(())
                } else {
                    anyhow::bail!("could not load the embedding model after download attempt")
                }
            }
            FixAction::PruneProjects => {
                let registry = engramdb::storage::FileRegistry::global()?;
                // `force: true` — the doctor already reported the findings and
                // the user just confirmed this fix, so don't double-prompt.
                crate::commands::run_projects(
                    dir,
                    &registry,
                    Some(ProjectsCommand::Prune { force: true }),
                    formatter,
                    &crate::prompter::InquirePrompter,
                )
                .await
            }
        }
    }
}

/// Map a finished environment report to the de-duplicated list of fixes to offer.
fn collect_fix_actions(result: &EnvironmentDoctorResult) -> Vec<FixAction> {
    let mut actions: Vec<FixAction> = Vec::new();
    let mut push = |a: FixAction| {
        if !actions.contains(&a) {
            actions.push(a);
        }
    };

    for check in result.all_checks() {
        let warn = check.status == Some(CheckStatus::Warn);
        match check.name.as_str() {
            "Store initialized" if !check.passed => push(FixAction::Init),
            "Store health" if !check.passed => push(FixAction::Reindex {
                embeddings_only: false,
            }),
            "Manifest stats" if warn => push(FixAction::Reindex {
                embeddings_only: false,
            }),
            "Embedding model identity" if !check.passed || warn => push(FixAction::Reindex {
                embeddings_only: true,
            }),
            "Embedding model cache" if warn => push(FixAction::DownloadEmbedding),
            "Registered projects" if warn => push(FixAction::PruneProjects),
            _ => {}
        }
    }

    // A full reindex re-embeds too, so it supersedes an embeddings-only one.
    if actions.contains(&FixAction::Reindex {
        embeddings_only: false,
    }) {
        actions.retain(|a| {
            *a != FixAction::Reindex {
                embeddings_only: true,
            }
        });
    }
    actions
}

/// Offer (and apply) fixes for the issues found in `result`.
///
/// Interactivity follows the same rule as `add`: prompt only when stdout is a
/// terminal and output isn't JSON. In non-interactive contexts nothing is
/// changed unless `--yes` was passed, in which case the safe fixes are applied
/// without prompting.
async fn apply_fixes(
    dir: &Path,
    global: bool,
    result: &EnvironmentDoctorResult,
    yes: bool,
    prompter: &dyn Prompter,
    formatter: &OutputFormatter,
) -> Result<()> {
    let actions = collect_fix_actions(result);
    if actions.is_empty() {
        formatter.print_success("Nothing to fix.");
        return Ok(());
    }

    let interactive = !yes && std::io::stdout().is_terminal() && !formatter.is_json();
    if !interactive && !yes {
        formatter.print_warning(&format!(
            "{} fixable issue(s) found. Re-run with `--fix --yes` to apply them:",
            actions.len()
        ));
        for action in &actions {
            formatter.print_message(&format!("  - {}", action.prompt()));
        }
        return Ok(());
    }

    for action in actions {
        let apply = if yes {
            true
        } else {
            prompter.confirm(action.prompt(), true)?
        };
        if apply {
            action.apply(dir, global, formatter).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputFormatter;
    use crate::prompter::MockPrompter;
    use engramdb::ops::EnvironmentCheck;
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    /// `fix` is false in these tests, so the prompter is never consulted.
    fn noop_prompter() -> MockPrompter {
        MockPrompter::new(vec![])
    }

    #[tokio::test]
    async fn test_doctor_store_healthy() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_doctor_store_global_targets_global_store() {
        let _lock = engramdb::storage::test_support::acquire_global_test_lock().await;
        let global = MemoryStore::open_global().await.unwrap();
        let mem = Memory::new(
            MemoryType::Decision,
            "Global",
            "Content",
            Provenance::human(),
        );
        global.create(&mem).await.unwrap();

        // `dir` points at an uninitialized project; --global must ignore it
        // and check the (healthy) global store instead.
        let temp_dir = TempDir::new().unwrap();
        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            true,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_ok(), "doctor --global failed: {:?}", result);

        // Without --global the project store is uninitialized → error.
        let project = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(project.is_err());
    }

    #[tokio::test]
    async fn test_doctor_store_with_orphan() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Write orphaned file
        let orphan_path = temp_dir
            .path()
            .join(".engramdb")
            .join("memories")
            .join("orphan-001.md");
        tokio::fs::write(&orphan_path, "---\nid: orphan-001\n---\n")
            .await
            .unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        // Unhealthy store (orphaned file) → Err so the process exits non-zero.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_doctor_environment_no_store() {
        let temp_dir = TempDir::new().unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            None,
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        // "Store initialized: not found" is a hard failure (passed=false),
        // so the environment check must exit non-zero. It still prints the
        // full report rather than erroring out early.
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_doctor_environment_with_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        // An initialized, healthy store must exit 0. Advisory findings (binary
        // not on PATH, untracked/legacy embedding fingerprint, .mcp.json not
        // configured) render as warnings now and never flip the exit code, so
        // the outcome is deterministic regardless of host environment.
        let result = run_doctor(
            temp_dir.path(),
            false,
            None,
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_ok(), "fresh init must pass doctor: {:?}", result);
    }

    #[tokio::test]
    async fn test_run_doctor_store_subcommand_fails_no_store() {
        let temp_dir = TempDir::new().unwrap();
        // No init — store does not exist

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_doctor_environment_no_store_exits_nonzero() {
        let temp_dir = TempDir::new().unwrap();
        // No init — the report still renders gracefully (no panic, full
        // output), but the missing store is a hard failure → Err.

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            None,
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_doctor_store_healthy_json() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        store.create(&mem).await.unwrap();

        // JSON formatter — exercises the json output path
        let formatter = OutputFormatter::new(None, true, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_doctor_store_with_stale_entries() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        let id = store.create(&mem).await.unwrap();

        // Delete the file behind the store's back to create a stale entry
        let file_path = temp_dir
            .path()
            .join(".engramdb")
            .join("memories")
            .join(format!("{}.md", id));
        tokio::fs::remove_file(&file_path).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        // Unhealthy store (stale index entry) → Err so the process exits
        // non-zero.
        assert!(result.is_err());
    }

    fn check(name: &str, passed: bool, status: Option<CheckStatus>) -> EnvironmentCheck {
        EnvironmentCheck {
            name: name.to_string(),
            passed,
            message: String::new(),
            suggestion: None,
            details: vec![],
            status,
        }
    }

    fn result_with(checks: Vec<EnvironmentCheck>) -> EnvironmentDoctorResult {
        let all_passed = checks.iter().all(|c| c.passed);
        EnvironmentDoctorResult {
            sections: vec![DoctorSection {
                name: "test".to_string(),
                checks,
                subsections: vec![],
            }],
            all_passed,
            store_check: None,
        }
    }

    #[test]
    fn collect_fix_actions_maps_issues_and_dedups_reindex() {
        let result = result_with(vec![
            check("Store health", false, None),
            check("Manifest stats", true, Some(CheckStatus::Warn)),
            check("Embedding model identity", true, Some(CheckStatus::Warn)),
            check("Embedding model cache", true, Some(CheckStatus::Warn)),
            check("Registered projects", true, Some(CheckStatus::Warn)),
        ]);
        let actions = collect_fix_actions(&result);
        // A full reindex re-embeds too, so the embeddings-only variant is dropped.
        assert!(actions.contains(&FixAction::Reindex {
            embeddings_only: false
        }));
        assert!(!actions.contains(&FixAction::Reindex {
            embeddings_only: true
        }));
        assert!(actions.contains(&FixAction::DownloadEmbedding));
        assert!(actions.contains(&FixAction::PruneProjects));
        // Each action appears once.
        assert_eq!(
            actions.len(),
            actions
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn collect_fix_actions_embeddings_only_without_full_reindex() {
        let result = result_with(vec![check(
            "Embedding model identity",
            true,
            Some(CheckStatus::Warn),
        )]);
        let actions = collect_fix_actions(&result);
        assert_eq!(
            actions,
            vec![FixAction::Reindex {
                embeddings_only: true
            }]
        );
    }

    #[test]
    fn collect_fix_actions_init_when_not_set_up() {
        let result = result_with(vec![check("Store initialized", false, None)]);
        assert_eq!(collect_fix_actions(&result), vec![FixAction::Init]);
    }

    #[test]
    fn collect_fix_actions_empty_when_healthy() {
        let result = result_with(vec![
            check("Store health", true, None),
            check("Registered projects", true, None),
        ]);
        assert!(collect_fix_actions(&result).is_empty());
    }

    #[tokio::test]
    async fn test_run_doctor_environment_with_store_json() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // JSON formatter + environment (None subcommand). Like the pretty
        // variant above, an initialized healthy store must exit 0 — advisory
        // checks render as warnings and do not gate the exit code.
        let formatter = OutputFormatter::new(None, true, true);
        let result = run_doctor(
            temp_dir.path(),
            false,
            None,
            false,
            false,
            &noop_prompter(),
            &formatter,
        )
        .await;
        assert!(result.is_ok(), "fresh init must pass doctor: {:?}", result);
    }
}
