//! Doctor (health check) command.

use crate::app::DoctorCommand;
use crate::output::{short_id, OutputFormatter};
use anyhow::Result;
use engramdb::ops::{doctor, doctor_environment};
use engramdb::storage::MemoryStore;
use std::path::Path;

/// Run doctor with optional subcommand dispatch.
///
/// - `None` → full environment diagnostics
/// - `Some(DoctorCommand::Store)` → fast store-only health check
pub async fn run_doctor(
    dir: &Path,
    global: bool,
    command: Option<DoctorCommand>,
    formatter: &OutputFormatter,
) -> Result<()> {
    match command {
        Some(DoctorCommand::Store) => run_store_check(dir, global, formatter).await,
        None => run_environment_check(dir, global, formatter).await,
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
    // `all_passed` only reflects hard failures (`passed == false`): checks
    // rendered as Warn/Info carry `passed == true`, so warnings never flip
    // the exit code — only real failures exit non-zero.
    if !result.all_passed {
        anyhow::bail!("environment check found failing checks");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputFormatter;
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

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
            &formatter,
        )
        .await;
        assert!(result.is_ok(), "doctor --global failed: {:?}", result);

        // Without --global the project store is uninitialized → error.
        let project = run_doctor(
            temp_dir.path(),
            false,
            Some(DoctorCommand::Store),
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
        let result = run_doctor(temp_dir.path(), false, None, &formatter).await;
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
        let result = run_doctor(temp_dir.path(), false, None, &formatter).await;
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
        let result = run_doctor(temp_dir.path(), false, None, &formatter).await;
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
            &formatter,
        )
        .await;
        // Unhealthy store (stale index entry) → Err so the process exits
        // non-zero.
        assert!(result.is_err());
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
        let result = run_doctor(temp_dir.path(), false, None, &formatter).await;
        assert!(result.is_ok(), "fresh init must pass doctor: {:?}", result);
    }
}
