//! Handler for `engramdb projects repair`.
//!
//! Re-keys a project whose ID drifted out from under its registry entry (see
//! [`engramdb::ops::repair`]), then rebuilds its index so the memories that
//! went missing come back.

use crate::engine::engine_for_project;
use crate::output::OutputFormatter;
use crate::prompter::Prompter;
use anyhow::{bail, Result};
use engramdb::daemon::{DaemonCell, DaemonPolicy};
use engramdb::ops::{self, repair::RepairReport};
use engramdb::storage::{MemoryStore, RegistryBackend};
use engramdb::types::EmbeddingBackend;
use std::path::Path;
use std::sync::Arc;

/// Run `engramdb projects repair`.
#[allow(clippy::too_many_arguments)]
pub async fn run_repair(
    dir: &Path,
    registry: &dyn RegistryBackend,
    force: bool,
    no_index: bool,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
    embedding_backend: Option<EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    let json_mode = formatter.is_json();

    let Some(plan) = ops::repair::plan_repair(registry, dir).await? else {
        if json_mode {
            println!(
                "{}",
                serde_json::json!({ "repaired": false, "reason": "nothing_to_repair" })
            );
        } else {
            formatter.print_success("Registration is consistent — nothing to repair.");
        }
        return Ok(());
    };

    if !json_mode {
        print_plan(formatter, &plan);
    }

    if !force {
        // JSON is machine-consumed: never prompt (mirrors `projects prune`).
        if json_mode {
            bail!("projects repair requires confirmation; re-run with --force in JSON mode");
        }
        if !prompter.confirm("Repair this registration?", true)? {
            formatter.print_message("Aborted.");
            return Ok(());
        }
    }

    let Some(report) = ops::repair::repair_project_id(registry, dir).await? else {
        // The drift was resolved between the plan and the repair (a concurrent
        // run). Nothing to do, and nothing went wrong.
        if json_mode {
            println!(
                "{}",
                serde_json::json!({ "repaired": false, "reason": "nothing_to_repair" })
            );
        } else {
            formatter.print_success("Registration is consistent — nothing to repair.");
        }
        return Ok(());
    };

    // The live ID's index is empty by construction — that is the symptom the
    // user came here for — so rebuilding it is part of the repair, not an
    // optional extra.
    let mut indexed = None;
    let mut embedded = None;
    let mut warnings: Vec<String> = Vec::new();
    if !no_index {
        let store = MemoryStore::open(dir).await?;
        let cache = ops::ProviderCache::new();
        let engine =
            engine_for_project(store.clone(), embedding_backend, cell, policy, &cache).await;
        let result = ops::reindex(&store, Some(&engine), false).await?;
        indexed = Some(result.indexed);
        embedded = Some(result.embedded);
        warnings.extend(result.warnings);
        if !result.errors.is_empty() {
            warnings.push(format!(
                "{} memory(ies) failed to embed and will be missed by semantic search",
                result.errors.len()
            ));
        }
    }

    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "repaired": true,
                "path": report.path.display().to_string(),
                "old_id": report.old_id,
                "new_id": report.new_id,
                "personal_migrated": report.personal_migrated,
                "personal_superseded": report.personal_superseded,
                "removed_duplicate_entry": report.removed_duplicate_entry,
                "reparented_children": report.reparented_children,
                "old_data_dir_removed": report.old_data_dir_removed,
                "no_index": no_index,
                "indexed": indexed,
                "embedded": embedded,
                "warnings": warnings,
            })
        );
        return Ok(());
    }

    formatter.print_success(&format!(
        "Re-keyed {} from {} to {}.",
        report.path.display(),
        report.old_id,
        report.new_id
    ));
    if report.personal_migrated > 0 {
        formatter.print_success(&format!(
            "Migrated {} personal memory(ies) to the live data directory.",
            report.personal_migrated
        ));
    }
    if report.personal_superseded > 0 {
        formatter.print_message(&format!(
            "  {} personal memory(ies) were already present in a newer form and were dropped.",
            report.personal_superseded
        ));
    }
    if report.removed_duplicate_entry {
        formatter.print_success("Removed the duplicate registry entry for this path.");
    }
    if !report.reparented_children.is_empty() {
        formatter.print_success(&format!(
            "Re-pointed {} sub-project(s) at the new ID: {}",
            report.reparented_children.len(),
            report.reparented_children.join(", ")
        ));
    }
    match (indexed, embedded) {
        (Some(i), Some(e)) => {
            formatter.print_success(&format!("Rebuilt the index: {i} indexed, {e} embedded."))
        }
        _ => formatter
            .print_hint("Memories stay missing from queries until you run `engramdb reindex`."),
    }
    for warning in &warnings {
        formatter.print_warning(warning);
    }
    if report.old_data_dir_removed {
        formatter.print_message(
            "  Removed the old data directory. Its usage history (stats events) is not carried \
             over and is gone; memories and vectors are not affected.",
        );
    }

    Ok(())
}

/// The blast radius, before anything is touched.
fn print_plan(formatter: &OutputFormatter, plan: &RepairReport) {
    formatter.print_warning(&format!(
        "{} is registered under project ID {} but now hashes to {}.",
        plan.path.display(),
        plan.old_id,
        plan.new_id
    ));
    formatter.print_message("This will:");
    formatter.print_message(
        "  - re-key the registry entry (subscriptions and worktree links preserved)",
    );
    if plan.personal_migrated > 0 || plan.personal_superseded > 0 {
        formatter.print_message(&format!(
            "  - migrate {} personal memory(ies) to the live data directory{}",
            plan.personal_migrated,
            if plan.personal_superseded > 0 {
                format!(
                    " ({} already superseded by a newer copy)",
                    plan.personal_superseded
                )
            } else {
                String::new()
            }
        ));
    }
    if plan.removed_duplicate_entry {
        formatter.print_message("  - remove the duplicate registry entry for this path");
    }
    if !plan.reparented_children.is_empty() {
        formatter.print_message(&format!(
            "  - re-point {} sub-project(s) at the new ID",
            plan.reparented_children.len()
        ));
    }
    formatter.print_message(&format!(
        "  - delete the old data directory ({}) once its memories have been moved",
        plan.old_id
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use crate::prompter::MockPrompter;
    use engramdb::storage::InMemoryRegistry;
    use tempfile::TempDir;

    fn fmt(json: bool) -> OutputFormatter {
        // Explicit: with `None`, a non-TTY stdout resolves to JSON and the
        // interactive tests would take the never-prompt branch.
        let format = if json {
            OutputFormat::Json
        } else {
            OutputFormat::Pretty
        };
        OutputFormatter::new(Some(format), json, true)
    }

    async fn run(
        dir: &Path,
        registry: &InMemoryRegistry,
        force: bool,
        prompter: &MockPrompter,
        json: bool,
    ) -> Result<()> {
        run_repair(
            dir,
            registry,
            force,
            true, // no_index: keep the unit tests off the model-loading path
            &fmt(json),
            prompter,
            None,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await
    }

    /// Init, then re-key the entry to a stale ID — what adding a git remote
    /// after `init` produces.
    async fn drift(registry: &InMemoryRegistry, dir: &Path) -> String {
        let store = MemoryStore::init(dir, registry).await.unwrap();
        let live = store.project_id.clone();
        let mut reg = registry.load().await.unwrap();
        reg.projects
            .iter_mut()
            .find(|e| e.project_id == live)
            .unwrap()
            .project_id = "stale00000000000".to_string();
        registry.save(&reg).await.unwrap();
        live
    }

    #[tokio::test]
    async fn repairs_after_confirmation() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live = drift(&registry, tmp.path()).await;

        run(
            tmp.path(),
            &registry,
            false,
            &MockPrompter::new(vec!["true"]),
            false,
        )
        .await
        .unwrap();

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.projects[0].project_id, live);
    }

    #[tokio::test]
    async fn declining_leaves_the_registry_untouched() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        drift(&registry, tmp.path()).await;

        run(
            tmp.path(),
            &registry,
            false,
            &MockPrompter::new(vec!["false"]),
            false,
        )
        .await
        .unwrap();

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects[0].project_id, "stale00000000000");
    }

    #[tokio::test]
    async fn a_consistent_project_is_a_no_op_and_never_prompts() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(tmp.path(), &registry).await.unwrap();

        let prompter = MockPrompter::new(vec![]);
        run(tmp.path(), &registry, false, &prompter, false)
            .await
            .unwrap();
        assert_eq!(prompter.prompt_count(), 0);
    }

    #[tokio::test]
    async fn json_without_force_refuses_to_prompt() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        drift(&registry, tmp.path()).await;

        let err = run(
            tmp.path(),
            &registry,
            false,
            &MockPrompter::new(vec![]),
            true,
        )
        .await
        .expect_err("JSON mode must refuse to prompt");
        assert!(format!("{err}").contains("--force"));
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects[0].project_id, "stale00000000000");
    }

    #[tokio::test]
    async fn force_skips_the_prompt() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live = drift(&registry, tmp.path()).await;

        let prompter = MockPrompter::new(vec![]);
        run(tmp.path(), &registry, true, &prompter, true)
            .await
            .unwrap();
        assert_eq!(prompter.prompt_count(), 0);
        assert_eq!(registry.load().await.unwrap().projects[0].project_id, live);
    }
}
