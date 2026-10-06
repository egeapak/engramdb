//! Per-session record of the memories the hooks already injected.
//!
//! Claude Code keeps every hook injection in the conversation and re-reads it
//! on every later turn. Re-injecting a memory the session already has costs
//! those tokens again on every turn and adds nothing, so the context hooks
//! skip memories listed here and spend the budget on new ones (and on full
//! bodies, which saves the `get` round-trip a cut preview used to cause).
//!
//! One append-only file per session, one memory id per line, at
//! `.engramdb/state/hook_seen/<session_id>`. Appends need no lock: two hooks
//! of one session appending at once can at worst both record an id, which
//! reads back the same. The record is advisory: a read or write failure means
//! "nothing seen yet", so a hook re-injects rather than hides a memory.
//!
//! A record is only true while the conversation still holds what it lists.
//! SessionStart (a new, cleared or compacted context) and PreCompact clear
//! it; SessionEnd deletes it and prunes records of sessions that never
//! ended cleanly.

use crate::error::Result;
use crate::state_file::{append_state_file, read_state_file};
use crate::transcripts::is_valid_session_id;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Records older than this are pruned on SessionEnd: their session is over.
const PRUNE_AFTER: Duration = Duration::from_secs(2 * 24 * 60 * 60);

fn seen_dir(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("hook_seen")
}

/// `None` for a session id that is not safe to use as a file name.
fn seen_path(project_dir: &Path, session_id: &str) -> Option<PathBuf> {
    is_valid_session_id(session_id).then(|| seen_dir(project_dir).join(session_id))
}

/// The memory ids this session's hooks already injected. Empty when the
/// session id is invalid, nothing was recorded, or the record cannot be read.
pub fn seen_ids(project_dir: &Path, session_id: &str) -> HashSet<String> {
    let Some(path) = seen_path(project_dir, session_id) else {
        return HashSet::new();
    };
    match read_state_file(&path) {
        Ok(Some(text)) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Ok(None) => HashSet::new(),
        Err(e) => {
            tracing::warn!(
                "hook record {} unreadable, treating as empty: {e}",
                path.display()
            );
            HashSet::new()
        }
    }
}

/// Record that this session's hooks injected `ids`.
pub fn mark_seen(project_dir: &Path, session_id: &str, ids: &[String]) -> Result<()> {
    let Some(path) = seen_path(project_dir, session_id) else {
        return Ok(());
    };
    let mut lines = String::new();
    for id in ids.iter().filter(|id| !id.is_empty() && !id.contains('\n')) {
        lines.push_str(id);
        lines.push('\n');
    }
    append_state_file(&path, &lines)
}

/// Forget what this session's hooks injected (its context was reset).
pub fn clear_seen(project_dir: &Path, session_id: &str) -> Result<()> {
    let Some(path) = seen_path(project_dir, session_id) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Delete records not written to for [`PRUNE_AFTER`]. Returns how many.
pub fn prune_stale(project_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(seen_dir(project_dir)) else {
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

    #[test]
    fn records_reads_and_clears_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert!(seen_ids(p, "s-1").is_empty());
        mark_seen(p, "s-1", &["a".into(), "b".into()]).unwrap();
        mark_seen(p, "s-1", &["b".into(), "c".into()]).unwrap();
        mark_seen(p, "s-2", &["z".into()]).unwrap();
        let got = seen_ids(p, "s-1");
        assert_eq!(got.len(), 3);
        assert!(got.contains("a") && got.contains("c"));
        assert_eq!(seen_ids(p, "s-2").len(), 1, "sessions are separate");
        clear_seen(p, "s-1").unwrap();
        assert!(seen_ids(p, "s-1").is_empty());
        clear_seen(p, "s-1").unwrap(); // clearing twice is fine
    }

    #[test]
    fn unsafe_session_ids_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        mark_seen(dir.path(), "../escape", &["a".into()]).unwrap();
        assert!(seen_ids(dir.path(), "../escape").is_empty());
        assert!(!dir.path().join(".engramdb").exists(), "nothing written");
    }
}
