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

/// What the user decided about a reviewed session.
///
/// Distinct from `memories_created == 0`, which conflates two different
/// things: a session genuinely holding nothing, and a session whose
/// candidates the user actively declined. Only the latter should be treated
/// as settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarvestDecision {
    /// Memories were saved from this session.
    Harvested,
    /// Reviewed and deliberately passed over.
    Skipped,
    /// Looked at, decision postponed — still offered by `list`.
    Deferred,
}

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
    /// The recorded decision.
    ///
    /// `None` in ledgers written before this field existed; [`Self::decision`]
    /// infers it rather than forcing a migration. This is a plain JSON file,
    /// not the LanceDB table, so `manifest::CURRENT_SCHEMA_VERSION` is not
    /// involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<HarvestDecision>,
    /// Free-text note, e.g. why a session was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The archived transcript, when one was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<crate::transcript_archive::ArchiveRef>,
}

impl HarvestEntry {
    /// The decision, inferring one for pre-`decision` ledger entries.
    pub fn decision(&self) -> HarvestDecision {
        self.decision.unwrap_or({
            if self.memories_created > 0 {
                HarvestDecision::Harvested
            } else {
                HarvestDecision::Skipped
            }
        })
    }

    /// Whether this entry settles the session, i.e. `list` should stop
    /// offering it. `Deferred` deliberately does not.
    pub fn is_settled(&self) -> bool {
        self.decision() != HarvestDecision::Deferred
    }
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

/// Is this session settled — i.e. should `list` stop offering it?
///
/// An entry that only records an archive (written by the SessionEnd hook for
/// a session nobody has reviewed yet) is `Deferred` and deliberately does
/// **not** count: archiving a transcript must never make it invisible to the
/// very command that exists to review it.
pub fn is_harvested(project_dir: &Path, session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    read_harvested(project_dir)
        .get(session_id)
        .is_some_and(|e| e.is_settled())
}

/// Record a review decision for `session_id`.
///
/// Re-recording overwrites, so a deliberate re-harvest updates rather than
/// duplicates. Any archive already attached to the entry is carried over —
/// the decision and the archive are written by different code paths (the
/// harvest flow and the SessionEnd hook) and neither may clobber the other.
pub fn mark_harvested(
    project_dir: &Path,
    session_id: &str,
    memory_ids: &[String],
    decision: HarvestDecision,
    note: Option<String>,
) -> Result<HarvestEntry> {
    if session_id.is_empty() {
        return Err(crate::error::StorageError::Validation(
            "cannot record a harvest for an empty session id".to_string(),
        ));
    }
    let _lock = lock_ledger(project_dir);
    let mut map = read_harvested(project_dir);
    let archive = map.get(session_id).and_then(|e| e.archive.clone());
    let entry = HarvestEntry {
        harvested_at: Utc::now(),
        memories_created: memory_ids.len(),
        memory_ids: memory_ids.to_vec(),
        decision: Some(decision),
        note,
        archive,
    };
    map.insert(session_id.to_string(), entry.clone());
    prune_stale(&mut map);
    write_harvested(project_dir, &map)?;
    Ok(entry)
}

/// Attach an archive to a session's entry, creating a `Deferred` entry when
/// the session has not been reviewed yet.
///
/// Symmetric to [`mark_harvested`]: an existing decision is preserved, so
/// archiving a session that was already harvested does not reopen it.
pub fn set_archive(
    project_dir: &Path,
    session_id: &str,
    archive: crate::transcript_archive::ArchiveRef,
) -> Result<()> {
    if session_id.is_empty() {
        return Ok(());
    }
    let _lock = lock_ledger(project_dir);
    let mut map = read_harvested(project_dir);
    match map.get_mut(session_id) {
        Some(entry) => entry.archive = Some(archive),
        None => {
            map.insert(
                session_id.to_string(),
                HarvestEntry {
                    harvested_at: Utc::now(),
                    memories_created: 0,
                    memory_ids: Vec::new(),
                    // Not a review — the session is still waiting for one.
                    decision: Some(HarvestDecision::Deferred),
                    note: None,
                    archive: Some(archive),
                },
            );
        }
    }
    prune_stale(&mut map);
    write_harvested(project_dir, &map)
}

/// Drop entries past the ledger's retention window.
///
/// Archives are pruned independently, by their own age/size budget in
/// `transcript_archive`, so an entry aging out here cannot strand a file
/// permanently — the orphan is reclaimed by that pass.
fn prune_stale(map: &mut HashMap<String, HarvestEntry>) {
    let cutoff = Utc::now() - Duration::days(PRUNE_AFTER_DAYS);
    map.retain(|_, e| e.harvested_at > cutoff);
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
        let entry = mark_harvested(
            dir,
            "s1",
            &["m1".into(), "m2".into()],
            HarvestDecision::Harvested,
            None,
        )
        .unwrap();
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
        let entry =
            mark_harvested(tmp.path(), "empty", &[], HarvestDecision::Skipped, None).unwrap();
        assert_eq!(entry.memories_created, 0);
        assert!(is_harvested(tmp.path(), "empty"));
    }

    #[test]
    fn re_harvest_overwrites_rather_than_duplicates() {
        let tmp = TempDir::new().unwrap();
        mark_harvested(tmp.path(), "s1", &[], HarvestDecision::Skipped, None).unwrap();
        mark_harvested(
            tmp.path(),
            "s1",
            &["m1".into()],
            HarvestDecision::Harvested,
            None,
        )
        .unwrap();
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
        mark_harvested(tmp.path(), "s1", &[], HarvestDecision::Skipped, None).unwrap();
        assert!(is_harvested(tmp.path(), "s1"));
    }

    #[test]
    fn pre_decision_entries_infer_their_decision() {
        // Ledgers written before `decision` existed must keep working without
        // a migration step — this is a plain JSON file, not the Lance table.
        let tmp = TempDir::new().unwrap();
        let path = ledger_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "old_harvested": {"harvested_at":"2026-07-01T00:00:00Z","memories_created":2},
              "old_empty":     {"harvested_at":"2026-07-01T00:00:00Z","memories_created":0}
            }"#,
        )
        .unwrap();

        let map = read_harvested(tmp.path());
        assert_eq!(map["old_harvested"].decision(), HarvestDecision::Harvested);
        assert_eq!(map["old_empty"].decision(), HarvestDecision::Skipped);
        assert!(map["old_empty"].is_settled());
    }

    #[test]
    fn archiving_alone_does_not_settle_a_session() {
        // The SessionEnd hook archives every session. If that counted as a
        // review, archiving would hide sessions from the very command meant
        // to review them.
        let tmp = TempDir::new().unwrap();
        set_archive(tmp.path(), "s1", sample_archive()).unwrap();

        let map = read_harvested(tmp.path());
        assert_eq!(map["s1"].decision(), HarvestDecision::Deferred);
        assert!(!map["s1"].is_settled());
        assert!(
            !is_harvested(tmp.path(), "s1"),
            "an archived-but-unreviewed session must still be offered"
        );
    }

    #[test]
    fn decision_and_archive_do_not_clobber_each_other() {
        let tmp = TempDir::new().unwrap();
        // Archive first (SessionEnd), then review (harvest flow).
        set_archive(tmp.path(), "s1", sample_archive()).unwrap();
        mark_harvested(
            tmp.path(),
            "s1",
            &["m1".into()],
            HarvestDecision::Harvested,
            None,
        )
        .unwrap();
        let map = read_harvested(tmp.path());
        assert!(map["s1"].archive.is_some(), "mark dropped the archive");
        assert_eq!(map["s1"].decision(), HarvestDecision::Harvested);

        // ...and the reverse order: a late archive must not reopen a decision.
        set_archive(tmp.path(), "s1", sample_archive()).unwrap();
        let map = read_harvested(tmp.path());
        assert_eq!(map["s1"].decision(), HarvestDecision::Harvested);
        assert_eq!(map["s1"].memories_created, 1);
    }

    #[test]
    fn skipped_and_deferred_differ_in_settlement() {
        let tmp = TempDir::new().unwrap();
        mark_harvested(tmp.path(), "skip", &[], HarvestDecision::Skipped, None).unwrap();
        mark_harvested(tmp.path(), "later", &[], HarvestDecision::Deferred, None).unwrap();
        assert!(is_harvested(tmp.path(), "skip"));
        assert!(
            !is_harvested(tmp.path(), "later"),
            "a deferred session must keep being offered"
        );
    }

    fn sample_archive() -> crate::transcript_archive::ArchiveRef {
        crate::transcript_archive::ArchiveRef {
            file_name: "s1.jsonl.zst".into(),
            bytes: 10,
            original_bytes: 100,
            sha256: "deadbeef".into(),
            archived_at: Utc::now(),
        }
    }

    #[test]
    fn empty_session_id_is_rejected() {
        let tmp = TempDir::new().unwrap();
        assert!(mark_harvested(tmp.path(), "", &[], HarvestDecision::Skipped, None).is_err());
        assert!(!is_harvested(tmp.path(), ""));
        assert!(!clear_harvested(tmp.path(), "").unwrap());
    }
}
