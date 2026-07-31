//! Record of which Claude Code sessions have already been harvested.
//!
//! `/engram:harvest` mines past session transcripts for durable knowledge.
//! Without a record of what it already looked at, every run would re-read
//! (and re-pay for) the entire history — and, worse, would keep re-proposing
//! candidates the user already declined.
//!
//! The **zero-yield** case is the one that makes this file load-bearing.
//! Sessions that produced no memory leave no other trace: memories created
//! from a session can be attributed after the fact, but a session that
//! legitimately held nothing worth saving is indistinguishable from one never
//! examined. Recording the examination itself — with its outcome — is what
//! makes a second run cheap.
//!
//! Storage mirrors [`crate::task_state`] exactly: a small JSON map under the
//! project's `.engramdb/state/`, guarded by an advisory `flock(2)` on a
//! sibling lock file and written atomically temp-then-rename. It is advisory
//! state, so a missing or malformed file reads as empty and never hard-fails
//! a harvest.
//!
//! Entries are keyed by session id and live under the **root** project, so a
//! worktree's sessions and the main checkout's sessions share one ledger —
//! matching the fact that they share one memory store.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entries older than this are pruned on write, bounding the file's growth
/// on a long-lived project. Well past any window in which re-harvesting an
/// old session would be useful.
const PRUNE_AFTER_DAYS: i64 = 365;

/// The outcome of harvesting one session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestEntry {
    /// When the session was harvested.
    pub harvested_at: DateTime<Utc>,
    /// How many memories the user accepted from it. `0` is a meaningful,
    /// deliberately-recorded value — see the module docs.
    #[serde(default)]
    pub memories_created: usize,
    /// Ids of the memories created from this session, for later attribution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_ids: Vec<String>,
}

/// Relative location of the ledger under the project root.
fn ledger_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("harvested_sessions.json")
}

/// Sync advisory lock guarding the read-modify-write cycle. Returns `None`
/// (proceed unlocked) only if the lock file itself can't be created — the
/// ledger is advisory and must never hard-fail a harvest.
fn lock_ledger(project_dir: &Path) -> Option<std::fs::File> {
    let lock_path = ledger_path(project_dir).with_extension("json.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Read the whole ledger. Missing or malformed files read as empty.
pub fn read_harvested(project_dir: &Path) -> HashMap<String, HarvestEntry> {
    match std::fs::read_to_string(ledger_path(project_dir)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Has this session already been harvested?
pub fn is_harvested(project_dir: &Path, session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    read_harvested(project_dir).contains_key(session_id)
}

/// Record that `session_id` was harvested, yielding `memory_ids` (possibly
/// empty). Re-recording a session overwrites the previous entry, so a
/// deliberate `--force` re-harvest updates rather than duplicates.
pub fn mark_harvested(
    project_dir: &Path,
    session_id: &str,
    memory_ids: &[String],
) -> Result<HarvestEntry> {
    if session_id.is_empty() {
        return Err(crate::error::StorageError::Validation(
            "cannot record a harvest for an empty session id".to_string(),
        ));
    }
    let _lock = lock_ledger(project_dir);
    let mut map = read_harvested(project_dir);
    let entry = HarvestEntry {
        harvested_at: Utc::now(),
        memories_created: memory_ids.len(),
        memory_ids: memory_ids.to_vec(),
    };
    map.insert(session_id.to_string(), entry.clone());

    let cutoff = Utc::now() - Duration::days(PRUNE_AFTER_DAYS);
    map.retain(|_, e| e.harvested_at > cutoff);

    write_harvested(project_dir, &map)?;
    Ok(entry)
}

/// Forget a session's harvest record so it is offered again.
pub fn clear_harvested(project_dir: &Path, session_id: &str) -> Result<bool> {
    if session_id.is_empty() {
        return Ok(false);
    }
    let _lock = lock_ledger(project_dir);
    let mut map = read_harvested(project_dir);
    let removed = map.remove(session_id).is_some();
    if removed {
        write_harvested(project_dir, &map)?;
    }
    Ok(removed)
}

fn write_harvested(project_dir: &Path, map: &HashMap<String, HarvestEntry>) -> Result<()> {
    let path = ledger_path(project_dir);
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
    fn roundtrip_and_clear() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        assert!(!is_harvested(dir, "s1"));
        let entry = mark_harvested(dir, "s1", &["m1".into(), "m2".into()]).unwrap();
        assert_eq!(entry.memories_created, 2);
        assert!(is_harvested(dir, "s1"));
        assert_eq!(read_harvested(dir)["s1"].memory_ids, vec!["m1", "m2"]);

        assert!(clear_harvested(dir, "s1").unwrap());
        assert!(!is_harvested(dir, "s1"));
        assert!(!clear_harvested(dir, "s1").unwrap());
    }

    #[test]
    fn zero_yield_session_is_still_recorded() {
        // The reason this module exists: a session that yielded nothing must
        // not be offered again on the next run.
        let tmp = TempDir::new().unwrap();
        let entry = mark_harvested(tmp.path(), "empty", &[]).unwrap();
        assert_eq!(entry.memories_created, 0);
        assert!(is_harvested(tmp.path(), "empty"));
    }

    #[test]
    fn re_harvest_overwrites_rather_than_duplicates() {
        let tmp = TempDir::new().unwrap();
        mark_harvested(tmp.path(), "s1", &[]).unwrap();
        mark_harvested(tmp.path(), "s1", &["m1".into()]).unwrap();
        let map = read_harvested(tmp.path());
        assert_eq!(map.len(), 1);
        assert_eq!(map["s1"].memories_created, 1);
    }

    #[test]
    fn malformed_ledger_reads_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = ledger_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert!(read_harvested(tmp.path()).is_empty());
        // And a write still succeeds over the garbage.
        mark_harvested(tmp.path(), "s1", &[]).unwrap();
        assert!(is_harvested(tmp.path(), "s1"));
    }

    #[test]
    fn empty_session_id_is_rejected() {
        let tmp = TempDir::new().unwrap();
        assert!(mark_harvested(tmp.path(), "", &[]).is_err());
        assert!(!is_harvested(tmp.path(), ""));
        assert!(!clear_harvested(tmp.path(), "").unwrap());
    }
}
