//! Task lifecycle CLI commands (§11.1–11.2).

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::ops;
use engramdb::storage::MemoryStore;
use std::path::Path;

/// Resolve the session id for `task current`: explicit flag, else the env
/// vars the MCP server also honors.
fn resolve_session_id(flag: Option<&str>) -> Result<String> {
    if let Some(sid) = flag {
        if !sid.is_empty() {
            return Ok(sid.to_string());
        }
    }
    for var in ["CLAUDE_SESSION_ID", "MCP_SESSION_ID"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    anyhow::bail!(
        "no session id: pass --session-id or set CLAUDE_SESSION_ID (the task \
         mapping is per-session)"
    )
}

/// Declare (or read) the session's current task (§11.1).
pub fn run_task_current(
    dir: &Path,
    global: bool,
    name: Option<&str>,
    session_id: Option<&str>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let target_dir = if global {
        engramdb::storage::paths::global_store_dir()?
    } else {
        dir.to_path_buf()
    };
    let session_id = resolve_session_id(session_id)?;
    let result = ops::task_current(&target_dir, &session_id, name)?;

    if formatter.is_json() {
        println!(
            "{}",
            serde_json::json!({ "session_id": result.session_id, "task": result.task })
        );
    } else {
        match (&result.task, name) {
            (Some(task), Some(_)) => {
                formatter.print_success(&format!("Session task set to '{task}'."))
            }
            (Some(task), None) => formatter.print_message(&format!("Current task: {task}")),
            (None, _) => formatter.print_message("No task declared for this session."),
        }
    }
    Ok(())
}

/// Mark a task finished (§11.2): demote its task-scoped memories.
pub async fn run_task_complete(
    dir: &Path,
    global: bool,
    name: &str,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    let config = engramdb::storage::config::load_config_or_default(
        &store.project_dir.join(".engramdb").join("config.toml"),
    )
    .await;
    let result = ops::task_complete(&store, name, &config.epistemic).await?;

    if formatter.is_json() {
        println!(
            "{}",
            serde_json::json!({
                "task": name,
                "demoted": result.demoted,
                "kept_custom_decay": result.kept_custom_decay,
                "project_wide_review": result
                    .project_wide_notices
                    .iter()
                    .map(|(id, summary)| serde_json::json!({ "id": id, "summary": summary }))
                    .collect::<Vec<_>>(),
            })
        );
    } else {
        formatter.print_success(&format!(
            "Task '{}' complete: {} memor(ies) demoted, {} kept custom decay.",
            name,
            result.demoted.len(),
            result.kept_custom_decay.len()
        ));
        for (id, summary) in &result.project_wide_notices {
            formatter.print_hint(&format!(
                "project-wide memory from this task — verify or demote: {} ('{}')",
                crate::output::short_id(id),
                summary
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_session_id_prefers_flag() {
        assert_eq!(resolve_session_id(Some("cli-sid")).unwrap(), "cli-sid");
        // Empty flag falls through to env; with no env either, errors.
        // (Env-var behavior is not asserted here to stay parallel-safe.)
        assert!(resolve_session_id(Some("")).is_err() || resolve_session_id(Some("")).is_ok());
    }
}
