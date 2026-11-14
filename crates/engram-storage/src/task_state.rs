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
/// (proceed unlocked) only if the lock file itself can't be opened — the
/// mapping is advisory state and must never hard-fail an operation.
///
/// Opened through `state_file`, like everything else under `state/`: a
/// symlink committed at the lock path is refused (with a warning) instead of
/// followed, which with a plain open created or touched whatever it named.
fn lock_mapping(project_dir: &Path) -> Option<std::fs::File> {
    let lock_path = mapping_path(project_dir).with_extension("json.lock");
    let file = match crate::state_file::open_lock_file(&lock_path) {
        Ok(file) => file,
        Err(e) => {
            tracing::warn!(
                "session tasks: could not open the lock file {} ({e}); proceeding unlocked \
                 (if that path is a symlink, remove it)",
                lock_path.display()
            );
            return None;
        }
    };
    file.lock().ok()?;
    Some(file)
}

/// What is on disk: the mapping, nothing, or something that does not parse.
enum Stored {
    Mapping(HashMap<String, TaskEntry>),
    Malformed,
}

fn load_mapping(project_dir: &Path) -> Stored {
    let path = mapping_path(project_dir);
    // Through the shared reader, which refuses to follow a symlink planted at
    // this path or to block on a FIFO — the same delivery `state_file`'s docs
    // describe for the write side, and this file is read by the unattended
    // hooks.
    match crate::state_file::read_state_file(&path) {
        Ok(Some(s)) => match serde_json::from_str(&s) {
            Ok(map) => Stored::Mapping(map),
            Err(e) => {
                warn_malformed_once(&path, &e);
                Stored::Malformed
            }
        },
        Ok(None) => Stored::Mapping(HashMap::new()),
        Err(e) => {
            tracing::warn!(
                "session tasks: refusing to read {} ({e}); it reads as empty",
                path.display()
            );
            Stored::Mapping(HashMap::new())
        }
    }
}

/// Warn once per process and path: every hook reads this file.
fn warn_malformed_once(path: &Path, e: &serde_json::Error) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let first = WARNED
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(path.to_path_buf()))
        .unwrap_or(false);
    if first {
        tracing::warn!(
            "session tasks: {} does not parse ({e}); it reads as empty and is kept aside \
             as session_tasks.json.malformed before the next write",
            path.display()
        );
    }
}

/// Read the whole session→task mapping. Missing or malformed files read as
/// empty (the mapping is advisory state, never a hard failure); a malformed
/// one is warned about once.
pub fn read_session_tasks(project_dir: &Path) -> HashMap<String, TaskEntry> {
    match load_mapping(project_dir) {
        Stored::Mapping(map) => map,
        Stored::Malformed => HashMap::new(),
    }
}

/// The mapping for a read-modify-write cycle. A malformed file is renamed to
/// `session_tasks.json.malformed` first: it used to read as empty and be
/// overwritten by the next write, silently discarding whatever it held.
fn read_for_write(project_dir: &Path) -> HashMap<String, TaskEntry> {
    match load_mapping(project_dir) {
        Stored::Mapping(map) => map,
        Stored::Malformed => {
            let path = mapping_path(project_dir);
            let aside = path.with_extension("json.malformed");
            // `rename` never dereferences its source, and replaces a planted
            // destination rather than following it.
            if let Err(e) = std::fs::rename(&path, &aside) {
                tracing::warn!(
                    "session tasks: could not keep the malformed {} aside ({e}); it will be \
                     overwritten",
                    path.display()
                );
            }
            HashMap::new()
        }
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
    let mut map = read_for_write(project_dir);
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
    let mut map = read_for_write(project_dir);
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
    let json = serde_json::to_string_pretty(map)
        .map_err(|e| crate::error::StorageError::Validation(e.to_string()))?;
    // Atomic temp-then-rename, same discipline as memory-file writes — and
    // symlink-refusing, because the temp path is predictable and `.engramdb/`
    // is committed. See `crate::state_file`.
    crate::state_file::write_state_json(&mapping_path(project_dir), &json)
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

    /// Same committed-symlink delivery as the harvest ledger — this file
    /// predates it and shares the state dir, so it is the same hole.
    #[test]
    #[cfg(unix)]
    fn a_planted_temp_symlink_cannot_redirect_a_task_write() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();

        let path = mapping_path(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("json.tmp")).unwrap();

        let result = set_current_task(dir, "s1", "some-task");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the mapping write landed on the symlink's target"
        );
        assert!(result.is_err(), "a redirected write reported success");
        assert!(!path.exists(), "the symlink was renamed onto the mapping");
    }

    /// A symlink committed at the lock path used to be followed: the open
    /// created (or touched) whatever it named.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_lock_file_is_not_followed() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join(".engramdb").join("state");
        std::fs::create_dir_all(&state).unwrap();
        let victim = tmp.path().join("victim");
        std::os::unix::fs::symlink(&victim, state.join("session_tasks.json.lock")).unwrap();

        // The mapping is advisory: the write still succeeds, unlocked.
        set_current_task(tmp.path(), "s", "t").unwrap();
        assert_eq!(current_task(tmp.path(), "s").as_deref(), Some("t"));
        assert!(
            !victim.exists(),
            "the lock open created the symlink's target"
        );
    }

    /// A file that does not parse read as empty and was overwritten by the
    /// next write, discarding whatever it held without a word.
    #[test]
    fn a_malformed_mapping_is_kept_aside_and_warned() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join(".engramdb").join("state");
        std::fs::create_dir_all(&state).unwrap();
        let path = state.join("session_tasks.json");
        std::fs::write(&path, "{\"s0\": {\"task\": \"half").unwrap();

        // Reading alone moves nothing.
        assert!(read_session_tasks(tmp.path()).is_empty());
        assert!(path.exists());
        assert!(!state.join("session_tasks.json.malformed").exists());

        set_current_task(tmp.path(), "s", "t").unwrap();
        assert_eq!(current_task(tmp.path(), "s").as_deref(), Some("t"));
        assert_eq!(
            std::fs::read_to_string(state.join("session_tasks.json.malformed")).unwrap(),
            "{\"s0\": {\"task\": \"half",
            "the unparseable bytes are kept for the user"
        );
    }
}
