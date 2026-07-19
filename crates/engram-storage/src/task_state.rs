//! Session→task association state (§11.1).
//!
//! Tasks are free-text names (`valid_while.origin_task`). A session declares
//! the task it is working on via `task_current`; the mapping lives in a small
//! JSON file under the project's `.engramdb/state/` dir, keyed by the
//! session id the front-end carries (provenance `session_id` /
//! `CLAUDE_SESSION_ID`). No global task registry — unknown tasks are just
//! strings. Writes use the same atomic temp-then-rename discipline as memory
//! files, serialized by a sync `flock(2)` on a sibling lock file so two
//! concurrent sessions can't lose each other's declarations.
//!
//! Entries carry a `declared_at` stamp. Hooks run in separate processes from
//! the MCP server, and the server's session id (a per-process fallback when
//! `CLAUDE_SESSION_ID` isn't in its environment) never matches the id in a
//! hook event — so hook-side readers use [`current_task_or_recent`], which
//! falls back to the most recently declared *fresh* mapping when their own
//! session id has no entry. Freshness is bounded so a long-dead session's
//! task can't leak into new sessions forever; SessionEnd additionally prunes
//! stale entries.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One session's declared task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    pub task: String,
    pub declared_at: DateTime<Utc>,
}

/// How long a mapping counts as "fresh" for the cross-process fallback in
/// [`current_task_or_recent`]. Sessions routinely span hours; a day bounds
/// the leak window for mappings whose owner never saw a SessionEnd.
pub const TASK_FALLBACK_MAX_AGE_HOURS: i64 = 24;

/// Entries older than this are pruned on SessionEnd housekeeping.
const PRUNE_AFTER_DAYS: i64 = 7;

/// Relative location of the mapping file under the project root.
fn mapping_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("session_tasks.json")
}

/// Sync advisory lock guarding the read-modify-write cycle. Returns `None`
/// (proceed unlocked) only if the lock file itself can't be created — the
/// mapping is advisory state and must never hard-fail an operation.
fn lock_mapping(project_dir: &Path) -> Option<std::fs::File> {
    let lock_path = mapping_path(project_dir).with_extension("json.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    file.lock_exclusive().ok()?;
    Some(file)
}

/// Read the whole session→task mapping. Missing or malformed files read as
/// empty (the mapping is advisory state, never a hard failure).
pub fn read_session_tasks(project_dir: &Path) -> HashMap<String, TaskEntry> {
    let path = mapping_path(project_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// The task the given session declared, if any.
pub fn current_task(project_dir: &Path, session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    read_session_tasks(project_dir)
        .get(session_id)
        .map(|e| e.task.clone())
}

/// The session's own task, else the most recently declared mapping younger
/// than [`TASK_FALLBACK_MAX_AGE_HOURS`] (from any session). This is the
/// hook-side reader: the MCP `task_current` tool records the mapping under
/// the *server process's* session id, which hook events can't know — the
/// freshness-bounded fallback is what makes "declare task_current to
/// surface yours" work under the default (plugin) install.
pub fn current_task_or_recent(project_dir: &Path, session_id: &str) -> Option<String> {
    let map = read_session_tasks(project_dir);
    if !session_id.is_empty() {
        if let Some(entry) = map.get(session_id) {
            return Some(entry.task.clone());
        }
    }
    let cutoff = Utc::now() - Duration::hours(TASK_FALLBACK_MAX_AGE_HOURS);
    map.values()
        .filter(|e| e.declared_at > cutoff)
        .max_by_key(|e| e.declared_at)
        .map(|e| e.task.clone())
}

/// Record (or overwrite) the session's declared task.
pub fn set_current_task(project_dir: &Path, session_id: &str, task: &str) -> Result<()> {
    if session_id.is_empty() {
        return Err(crate::error::StorageError::Validation(
            "cannot record a task for an empty session id".to_string(),
        ));
    }
    let _lock = lock_mapping(project_dir);
    let mut map = read_session_tasks(project_dir);
    map.insert(
        session_id.to_string(),
        TaskEntry {
            task: task.to_string(),
            declared_at: Utc::now(),
        },
    );
    write_session_tasks(project_dir, &map)
}

/// Clear the session's task association (SessionEnd housekeeping) and prune
/// entries stale for more than [`PRUNE_AFTER_DAYS`] — mappings whose owning
/// process died without a SessionEnd (e.g. the MCP server's synthetic
/// session id) would otherwise accumulate forever. Returns the task that
/// was mapped, if any.
pub fn clear_session_task(project_dir: &Path, session_id: &str) -> Result<Option<String>> {
    if session_id.is_empty() {
        return Ok(None);
    }
    let _lock = lock_mapping(project_dir);
    let mut map = read_session_tasks(project_dir);
    let removed = map.remove(session_id).map(|e| e.task);
    let before = map.len();
    let prune_cutoff = Utc::now() - Duration::days(PRUNE_AFTER_DAYS);
    map.retain(|_, e| e.declared_at > prune_cutoff);
    if removed.is_some() || map.len() != before {
        write_session_tasks(project_dir, &map)?;
    }
    Ok(removed)
}

fn write_session_tasks(project_dir: &Path, map: &HashMap<String, TaskEntry>) -> Result<()> {
    let path = mapping_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(map)
        .map_err(|e| crate::error::StorageError::Validation(e.to_string()))?;
    // Atomic temp-then-rename, same discipline as memory-file writes.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn mapping_roundtrip_and_clear() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        assert_eq!(current_task(dir, "s1"), None);

        set_current_task(dir, "s1", "epistemic-memory").unwrap();
        set_current_task(dir, "s2", "other-task").unwrap();
        assert_eq!(current_task(dir, "s1").as_deref(), Some("epistemic-memory"));
        assert_eq!(current_task(dir, "s2").as_deref(), Some("other-task"));

        // Overwrite is allowed (a session can switch tasks).
        set_current_task(dir, "s1", "new-task").unwrap();
        assert_eq!(current_task(dir, "s1").as_deref(), Some("new-task"));

        // Clear returns the mapped task and removes only that session.
        assert_eq!(
            clear_session_task(dir, "s1").unwrap().as_deref(),
            Some("new-task")
        );
        assert_eq!(current_task(dir, "s1"), None);
        assert_eq!(current_task(dir, "s2").as_deref(), Some("other-task"));
        // Clearing an unmapped session is a no-op.
        assert_eq!(clear_session_task(dir, "s1").unwrap(), None);
    }

    #[test]
    fn empty_session_id_rejected_or_none() {
        let tmp = TempDir::new().unwrap();
        assert!(set_current_task(tmp.path(), "", "task").is_err());
        assert_eq!(current_task(tmp.path(), ""), None);
        assert_eq!(clear_session_task(tmp.path(), "").unwrap(), None);
    }

    #[test]
    fn malformed_file_reads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".engramdb").join("state");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("session_tasks.json"), "not json").unwrap();
        assert!(read_session_tasks(tmp.path()).is_empty());
        // A write recovers the file.
        set_current_task(tmp.path(), "s", "t").unwrap();
        assert_eq!(current_task(tmp.path(), "s").as_deref(), Some("t"));
    }

    /// The hook-side reader falls back to the freshest foreign mapping when
    /// its own session id is unmapped (the MCP-tool → hook bridge), but a
    /// direct hit always wins, and stale foreign mappings never surface.
    #[test]
    fn recent_fallback_bridges_foreign_session_ids() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // No mappings at all → no task.
        assert_eq!(current_task_or_recent(dir, "hook-session"), None);

        // A fresh mapping under a foreign (MCP-process) id is found.
        set_current_task(dir, "mcp-synthetic-id", "epistemic-e2e").unwrap();
        assert_eq!(
            current_task_or_recent(dir, "hook-session").as_deref(),
            Some("epistemic-e2e")
        );

        // The session's own mapping wins over any fallback.
        set_current_task(dir, "hook-session", "own-task").unwrap();
        assert_eq!(
            current_task_or_recent(dir, "hook-session").as_deref(),
            Some("own-task")
        );

        // A stale foreign mapping (older than the freshness window) does not
        // surface. Backdate by rewriting the entry directly.
        clear_session_task(dir, "hook-session").unwrap();
        let mut map = read_session_tasks(dir);
        map.get_mut("mcp-synthetic-id").unwrap().declared_at =
            Utc::now() - Duration::hours(TASK_FALLBACK_MAX_AGE_HOURS + 1);
        write_session_tasks(dir, &map).unwrap();
        assert_eq!(current_task_or_recent(dir, "hook-session"), None);
    }

    /// SessionEnd prunes entries stale past the prune horizon even when the
    /// ending session itself has no mapping.
    #[test]
    fn clear_prunes_stale_entries() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        set_current_task(dir, "old", "ancient-task").unwrap();
        let mut map = read_session_tasks(dir);
        map.get_mut("old").unwrap().declared_at = Utc::now() - Duration::days(PRUNE_AFTER_DAYS + 1);
        write_session_tasks(dir, &map).unwrap();

        clear_session_task(dir, "unrelated").unwrap();
        assert!(read_session_tasks(dir).is_empty());
    }
}
