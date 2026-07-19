//! Session→task association state (§11.1).
//!
//! Tasks are free-text names (`valid_while.origin_task`). A session declares
//! the task it is working on via `task_current`; the mapping lives in a small
//! JSON file under the project's `.engramdb/state/` dir, keyed by the
//! session id the front-end carries (provenance `session_id` /
//! `CLAUDE_SESSION_ID`). No global task registry — unknown tasks are just
//! strings. Writes use the same atomic temp-then-rename discipline as memory
//! files.

use crate::error::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Relative location of the mapping file under the project root.
fn mapping_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("session_tasks.json")
}

/// Read the whole session→task mapping. Missing or malformed files read as
/// empty (the mapping is advisory state, never a hard failure).
pub fn read_session_tasks(project_dir: &Path) -> HashMap<String, String> {
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
    read_session_tasks(project_dir).get(session_id).cloned()
}

/// Record (or overwrite) the session's declared task.
pub fn set_current_task(project_dir: &Path, session_id: &str, task: &str) -> Result<()> {
    if session_id.is_empty() {
        return Err(crate::error::StorageError::Validation(
            "cannot record a task for an empty session id".to_string(),
        ));
    }
    let mut map = read_session_tasks(project_dir);
    map.insert(session_id.to_string(), task.to_string());
    write_session_tasks(project_dir, &map)
}

/// Clear the session's task association (SessionEnd housekeeping). Returns
/// the task that was mapped, if any.
pub fn clear_session_task(project_dir: &Path, session_id: &str) -> Result<Option<String>> {
    if session_id.is_empty() {
        return Ok(None);
    }
    let mut map = read_session_tasks(project_dir);
    let removed = map.remove(session_id);
    if removed.is_some() {
        write_session_tasks(project_dir, &map)?;
    }
    Ok(removed)
}

fn write_session_tasks(project_dir: &Path, map: &HashMap<String, String>) -> Result<()> {
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
}
