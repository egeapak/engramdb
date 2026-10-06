//! Per-session record of Bash commands that failed, for the "save what cost
//! you effort" prompt.
//!
//! The PostToolUseFailure hook records a key for a failed command; when a
//! later PostToolUse for the same key succeeds, the hook prompts once to save
//! the cause, and records that it prompted so the key never prompts again in
//! this session. A file edit in between settles the failure without a prompt:
//! fail, edit, pass is the normal red-green loop, not a hidden cause.
//!
//! One append-only file per session at `.engramdb/state/bash_retry/<session>`,
//! one `F <key>` (failed), `S <key>` (settled by an edit) or `P <key>`
//! (prompted) line per event, folded in order. Appends need
//! no lock: two hooks appending at once can at worst both record a line, which
//! reads back the same. The record is advisory: a read failure means "nothing
//! failed", so the hook stays silent rather than prompting wrongly.

use crate::error::Result;
use crate::state_file::{append_state_file, read_state_file};
use crate::transcripts::is_valid_session_id;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Records older than this are pruned on SessionEnd: their session is over.
const PRUNE_AFTER: Duration = Duration::from_secs(2 * 24 * 60 * 60);

/// A key longer than this is not recorded: it is not a command name.
const MAX_KEY_CHARS: usize = 200;

fn retry_dir(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("bash_retry")
}

/// `None` for a session id that is not safe to use as a file name.
fn retry_path(project_dir: &Path, session_id: &str) -> Option<PathBuf> {
    is_valid_session_id(session_id).then(|| retry_dir(project_dir).join(session_id))
}

fn usable(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= MAX_KEY_CHARS && !key.contains(['\n', '\r'])
}

fn append(project_dir: &Path, session_id: &str, tag: char, keys: &[String]) -> Result<()> {
    let Some(path) = retry_path(project_dir, session_id) else {
        return Ok(());
    };
    let mut lines = String::new();
    for key in keys.iter().filter(|k| usable(k)) {
        lines.push(tag);
        lines.push(' ');
        lines.push_str(key);
        lines.push('\n');
    }
    if lines.is_empty() {
        return Ok(());
    }
    append_state_file(&path, &lines)
}

/// Record that a command with these keys failed in this session.
pub fn record_failure(project_dir: &Path, session_id: &str, keys: &[String]) -> Result<()> {
    append(project_dir, session_id, 'F', keys)
}

/// Record that the hook prompted for these keys, so they never prompt again.
pub fn mark_prompted(project_dir: &Path, session_id: &str, keys: &[String]) -> Result<()> {
    append(project_dir, session_id, 'P', keys)
}

/// Settle every pending failure without prompting: the session edited files
/// after the failure, so a later success is a code fix, not a hidden cause.
/// A no-op (and no file) when nothing is pending.
pub fn settle_pending(project_dir: &Path, session_id: &str) -> Result<()> {
    let pending: Vec<String> = pending_failures(project_dir, session_id)
        .into_iter()
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    append(project_dir, session_id, 'S', &pending)
}

/// Keys that failed in this session and have not been prompted for yet.
/// Empty when the session id is invalid, nothing was recorded, or the record
/// cannot be read.
pub fn pending_failures(project_dir: &Path, session_id: &str) -> HashSet<String> {
    let Some(path) = retry_path(project_dir, session_id) else {
        return HashSet::new();
    };
    let text = match read_state_file(&path) {
        Ok(Some(text)) => text,
        Ok(None) => return HashSet::new(),
        Err(e) => {
            tracing::warn!(
                "retry record {} unreadable, treating as empty: {e}",
                path.display()
            );
            return HashSet::new();
        }
    };
    // Folded in order: a settle clears the failures before it, a prompt clears
    // them and blocks the key for the rest of the session.
    let mut failed = HashSet::new();
    let mut prompted = HashSet::new();
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("F", key)) if !prompted.contains(key) => {
                failed.insert(key.to_string());
            }
            Some(("S", key)) => {
                failed.remove(key);
            }
            Some(("P", key)) => {
                failed.remove(key);
                prompted.insert(key.to_string());
            }
            _ => {}
        }
    }
    failed
}

/// Delete this session's record (the session ended).
pub fn clear(project_dir: &Path, session_id: &str) -> Result<()> {
    let Some(path) = retry_path(project_dir, session_id) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Delete records not written to for [`PRUNE_AFTER`]. Returns how many.
pub fn prune_stale(project_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(retry_dir(project_dir)) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > PRUNE_AFTER);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn failure_is_pending_until_prompted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert!(pending_failures(p, "s-1").is_empty());
        record_failure(p, "s-1", &keys(&["scripts/seed_dev.py"])).unwrap();
        record_failure(p, "s-1", &keys(&["scripts/seed_dev.py", "pytest"])).unwrap();
        let pending = pending_failures(p, "s-1");
        assert_eq!(pending.len(), 2);
        assert!(pending.contains("scripts/seed_dev.py"));

        mark_prompted(p, "s-1", &keys(&["scripts/seed_dev.py"])).unwrap();
        assert_eq!(pending_failures(p, "s-1"), HashSet::from(["pytest".into()]));
        // A later failure of a prompted key does not prompt again.
        record_failure(p, "s-1", &keys(&["scripts/seed_dev.py"])).unwrap();
        assert!(!pending_failures(p, "s-1").contains("scripts/seed_dev.py"));
    }

    #[test]
    fn settle_pending_retires_failures_without_a_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        settle_pending(p, "s-1").unwrap();
        assert!(!p.join(".engramdb/state/bash_retry/s-1").exists());
        record_failure(p, "s-1", &keys(&["cargo test"])).unwrap();
        settle_pending(p, "s-1").unwrap();
        assert!(pending_failures(p, "s-1").is_empty());
        // A later failure of the same command is pending again: settling is
        // not prompting, so it does not block the key.
        record_failure(p, "s-1", &keys(&["cargo test"])).unwrap();
        assert!(pending_failures(p, "s-1").contains("cargo test"));
    }

    #[test]
    fn sessions_are_separate_and_clear_removes_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        record_failure(p, "s-1", &keys(&["make test"])).unwrap();
        assert!(pending_failures(p, "s-2").is_empty());
        clear(p, "s-1").unwrap();
        assert!(pending_failures(p, "s-1").is_empty());
        clear(p, "s-1").unwrap(); // missing record is fine
    }

    #[test]
    fn unsafe_session_ids_and_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        record_failure(p, "../escape", &keys(&["x"])).unwrap();
        assert!(!p.join(".engramdb/state/escape").exists());
        record_failure(p, "s-1", &keys(&["a\nF injected", ""])).unwrap();
        assert!(pending_failures(p, "s-1").is_empty());
    }
}
