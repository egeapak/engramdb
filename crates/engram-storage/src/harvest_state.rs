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
//! matching the fact that they share one memory store. Every function here
//! takes the directory to use verbatim; resolving the root is the caller's
//! job, and [`adopt_ledger`] is what folds a ledger an older version left at a
//! non-root path into the root's.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entries older than this are pruned on write, bounding the file's growth
/// on a long-lived project. Well past any window in which re-harvesting an
/// old session would be useful.
///
/// Applies only to entries that hold no archive — see [`prune_stale`].
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
    let file = match std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            // Proceeding unlocked is the documented fallback, but it must not
            // be silent: two concurrent read-modify-writes then drop an entry
            // with no trace, which reads exactly like a session that was
            // never marked.
            tracing::warn!(
                "could not open harvest ledger lock at {} ({e}); proceeding without it — \
                 a concurrent harvest could lose an entry",
                lock_path.display()
            );
            return None;
        }
    };
    if let Err(e) = file.lock() {
        tracing::warn!(
            "could not lock harvest ledger at {} ({e}); proceeding without it — \
             a concurrent harvest could lose an entry",
            lock_path.display()
        );
        return None;
    }
    Some(file)
}

/// Read the whole ledger. A missing file reads as empty.
///
/// A **malformed** one also reads as empty, but not silently: the bad file is
/// moved aside to `harvested_sessions.json.corrupt-<pid>` first. Without that
/// step the failure is unrecoverable and invisible — an empty read is
/// immediately written back by the next `mark_harvested`/`set_archive`,
/// destroying every review decision, memory attribution and archive
/// reference, and orphaning every archive on disk while it still counts
/// against the size budget. Deserialization is all-or-nothing, so a single
/// unparseable field from a future version would do it too.
///
/// Reading as empty (rather than failing) is still the right default — the
/// ledger is advisory and must never hard-fail a harvest — but the evidence
/// has to survive.
pub fn read_harvested(project_dir: &Path) -> HashMap<String, HarvestEntry> {
    let path = ledger_path(project_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    match serde_json::from_str(&raw) {
        Ok(map) => map,
        Err(e) => {
            let quarantine = path.with_extension(format!("json.corrupt-{}", std::process::id()));
            let moved = std::fs::rename(&path, &quarantine).is_ok();
            tracing::warn!(
                "harvest ledger at {} is unreadable ({e}); treating it as empty. {}",
                path.display(),
                if moved {
                    format!(
                        "The previous contents were kept at {}.",
                        quarantine.display()
                    )
                } else {
                    "The previous contents could NOT be preserved.".to_string()
                }
            );
            HashMap::new()
        }
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
    // Reject anything that is not a plain identifier, not just the empty
    // string. A ledger key is later joined into an archive path by
    // `harvest ledger rm` / `export`, so a key like `../../x` planted here
    // (this is reachable from the MCP `harvest_mark` tool) would aim a later,
    // entirely innocent command at a file outside the store.
    if !crate::transcripts::is_valid_session_id(session_id) {
        return Err(crate::error::StorageError::Validation(format!(
            "cannot record a harvest for session id {session_id:?}: expected a \
             plain identifier (letters, digits, '-', '_', '.') that is not a path"
        )));
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
    if !crate::transcripts::is_valid_session_id(session_id) {
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

/// Forget the archive reference for one or more sessions, keeping the review
/// record itself.
///
/// The inverse of [`set_archive`], and mandatory whenever an archive file is
/// deleted: an entry pointing at a file that eviction has already removed
/// makes `harvest ledger show` advertise a transcript that cannot be exported,
/// and turns `export` into a bare "no such file" instead of a clear
/// explanation. Takes a slice because the size-based prune pass evicts many
/// archives at once and should cost one ledger write, not one per file.
pub fn clear_archive_refs(project_dir: &Path, session_ids: &[String]) -> Result<()> {
    if session_ids.is_empty() {
        return Ok(());
    }
    let _lock = lock_ledger(project_dir);
    let mut map = read_harvested(project_dir);
    let mut touched = false;
    for id in session_ids {
        if let Some(entry) = map.get_mut(id) {
            touched |= entry.archive.take().is_some();
        }
    }
    if !touched {
        return Ok(());
    }
    prune_stale(&mut map);
    write_harvested(project_dir, &map)
}

/// Drop entries past the ledger's retention window, **except** ones that still
/// point at an archive.
///
/// An archive is only reachable *through* its ledger entry: `harvest show`,
/// `ledger export` and `ledger rm` all resolve a session id against this map
/// first, so dropping an entry whose file is still on disk makes that file
/// unreachable by every route while it keeps consuming the archive budget.
/// The two windows are independent and users are explicitly told to set
/// `archive_retention_days = 3650`, so "the archive pass will reclaim it"
/// holds only when that window is the shorter of the two — which is exactly
/// the configuration the docs steer people away from.
///
/// Exempting them does not unbound the file: `clear_archive_refs` runs on
/// every eviction, so an entry loses its exemption the moment its archive
/// goes, and the number of archives is itself bounded by
/// `archive_max_bytes` / `archive_retention_days`.
fn prune_stale(map: &mut HashMap<String, HarvestEntry>) {
    let cutoff = Utc::now() - Duration::days(PRUNE_AFTER_DAYS);
    map.retain(|_, e| e.archive.is_some() || e.harvested_at > cutoff);
}

/// Fold a ledger left at `sub_dir` into the root project's ledger at
/// `root_dir`, then move the old file aside.
///
/// Callers resolve the root themselves, and older versions did not: a project
/// linked with `engramdb projects link` wrote its ledger under its own path
/// while its archives already went to the root. Silently reading the root's
/// ledger from then on would lose every review decision recorded before the
/// fix — sessions re-offered, notes and memory attributions gone — so the old
/// file is merged rather than abandoned.
///
/// Merging (not moving) because both files can hold entries for the same
/// session: the SessionEnd hook has been writing archive references to the
/// root's ledger all along, while the review decisions landed in the
/// sub-project's. The old file is renamed rather than deleted — this runs
/// unattended from the SessionEnd hook, and nothing here is worth destroying
/// evidence over.
pub fn adopt_ledger(sub_dir: &Path, root_dir: &Path) -> Result<()> {
    if sub_dir == root_dir {
        return Ok(());
    }
    let legacy_path = ledger_path(sub_dir);
    if !legacy_path.exists() {
        return Ok(());
    }
    // Reads as empty (and quarantines the file) if it is corrupt, in which
    // case there is nothing to adopt and the rename below finds nothing.
    let legacy = read_harvested(sub_dir);

    if !legacy.is_empty() {
        let _lock = lock_ledger(root_dir);
        let mut map = read_harvested(root_dir);
        for (id, entry) in legacy {
            let merged = match map.get(&id) {
                Some(existing) => merge_entries(existing, &entry),
                None => entry,
            };
            map.insert(id, merged);
        }
        prune_stale(&mut map);
        write_harvested(root_dir, &map)?;
    }
    let _ = std::fs::rename(&legacy_path, legacy_path.with_extension("json.adopted"));
    Ok(())
}

/// Reconcile two records of the same session found in two ledgers.
///
/// A settled decision outranks a `Deferred` one regardless of timestamps: the
/// SessionEnd hook writes a `Deferred` entry for every session it archives, so
/// the root's copy is routinely *younger* than the review it must not
/// overwrite. Beyond that the newer record wins, and an archive reference is
/// kept from whichever side has one — the file it names is the same either
/// way, since archives have always been keyed by the root project.
fn merge_entries(root: &HarvestEntry, legacy: &HarvestEntry) -> HarvestEntry {
    let prefer_legacy = match (root.is_settled(), legacy.is_settled()) {
        (false, true) => true,
        (true, false) => false,
        _ => legacy.harvested_at > root.harvested_at,
    };
    let (base, other) = if prefer_legacy {
        (legacy, root)
    } else {
        (root, legacy)
    };
    HarvestEntry {
        archive: base.archive.clone().or_else(|| other.archive.clone()),
        ..base.clone()
    }
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
    fn clearing_an_archive_ref_keeps_the_review_record() {
        // Eviction deletes files, not reviews. If the decision were dropped
        // along with the archive, pruning old transcripts would silently
        // re-offer every session that had already been reviewed.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        mark_harvested(dir, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();
        set_archive(dir, "s1", sample_archive()).unwrap();
        assert!(read_harvested(dir)["s1"].archive.is_some());

        clear_archive_refs(dir, &["s1".to_string()]).unwrap();
        let entry = &read_harvested(dir)["s1"];
        assert!(entry.archive.is_none(), "file pointer must be dropped");
        assert_eq!(entry.decision, Some(HarvestDecision::Harvested));
        assert_eq!(entry.memory_ids, vec!["m1"]);
        assert!(is_harvested(dir, "s1"));
    }

    #[test]
    fn clearing_archive_refs_tolerates_unknown_and_empty_input() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        // A prune pass can evict an archive whose ledger entry already aged
        // out; that must not be an error, and must not create an entry.
        clear_archive_refs(dir, &["ghost".to_string()]).unwrap();
        clear_archive_refs(dir, &[]).unwrap();
        assert!(read_harvested(dir).is_empty());
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

    #[test]
    fn an_entry_still_holding_an_archive_outlives_the_age_window() {
        // The entry is the only index into the archive file — `show`,
        // `ledger export` and `ledger rm` all resolve through it — so aging it
        // out makes the file unreachable by every route while it keeps
        // consuming the archive budget. That is not hypothetical: the docs
        // tell users to set `archive_retention_days = 3650`, ten times this
        // window.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let long_ago = Utc::now() - Duration::days(PRUNE_AFTER_DAYS + 35);
        let mut seeded = HashMap::new();
        seeded.insert(
            "archived".to_string(),
            HarvestEntry {
                harvested_at: long_ago,
                memories_created: 1,
                memory_ids: vec!["m1".into()],
                decision: Some(HarvestDecision::Harvested),
                note: None,
                archive: Some(sample_archive()),
            },
        );
        seeded.insert(
            "no-archive".to_string(),
            HarvestEntry {
                harvested_at: long_ago,
                memories_created: 0,
                memory_ids: vec![],
                decision: Some(HarvestDecision::Skipped),
                note: None,
                archive: None,
            },
        );
        write_harvested(dir, &seeded).unwrap();

        // Any write runs the prune pass; the SessionEnd hook's `set_archive`
        // is the one that fires unattended on every session.
        set_archive(dir, "fresh", sample_archive()).unwrap();

        let after = read_harvested(dir);
        let entry = after
            .get("archived")
            .expect("an entry pointing at a live archive was pruned, orphaning the file");
        assert_eq!(
            entry.memory_ids,
            vec!["m1"],
            "the review record must survive"
        );
        assert!(
            !after.contains_key("no-archive"),
            "the age bound must still apply to entries with no file behind them"
        );
    }

    #[test]
    fn adopting_a_sub_project_ledger_keeps_both_sides_and_moves_the_file_aside() {
        // A project linked with `projects link` used to keep its own ledger
        // while its archives went to the root. Reading the root's ledger from
        // then on must not lose those decisions.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("child");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        mark_harvested(&sub, "only-sub", &[], HarvestDecision::Skipped, None).unwrap();
        mark_harvested(&root, "only-root", &[], HarvestDecision::Skipped, None).unwrap();

        adopt_ledger(&sub, &root).unwrap();

        let merged = read_harvested(&root);
        assert!(merged.contains_key("only-sub"), "sub-project record lost");
        assert!(merged.contains_key("only-root"), "root record clobbered");
        assert!(
            !ledger_path(&sub).exists(),
            "the adopted ledger must stop being written to"
        );
        assert!(
            ledger_path(&sub).with_extension("json.adopted").exists(),
            "the old file must be kept, not deleted"
        );

        // Idempotent: nothing left to adopt, and the root is untouched.
        adopt_ledger(&sub, &root).unwrap();
        assert_eq!(read_harvested(&root).len(), 2);
    }

    #[test]
    fn adoption_does_not_let_a_hook_deferral_undo_a_review() {
        // The SessionEnd hook writes a `Deferred` entry to the root for every
        // session it archives, so the root's copy is routinely *newer* than
        // the review recorded on the sub-project side. Timestamp alone would
        // reopen a session the user already settled.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("child");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        mark_harvested(&sub, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();
        set_archive(&root, "s1", sample_archive()).unwrap();

        adopt_ledger(&sub, &root).unwrap();

        let entry = &read_harvested(&root)["s1"];
        assert_eq!(entry.decision(), HarvestDecision::Harvested);
        assert_eq!(entry.memory_ids, vec!["m1"]);
        assert!(
            entry.archive.is_some(),
            "the archive reference must survive the merge"
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

#[cfg(test)]
mod corruption_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_corrupt_ledger_is_preserved_rather_than_overwritten() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        mark_harvested(
            dir,
            "keep-me",
            &["m1".into()],
            HarvestDecision::Harvested,
            None,
        )
        .unwrap();
        let path = ledger_path(dir);
        let original = std::fs::read_to_string(&path).unwrap();
        assert!(original.contains("keep-me"));

        // Truncate mid-JSON, as an interrupted write or a bad disk would.
        std::fs::write(&path, &original[..original.len() / 2]).unwrap();

        // Reads as empty — the ledger is advisory and must not hard-fail...
        assert!(read_harvested(dir).is_empty());

        // ...but the evidence survives, so the loss is recoverable.
        let quarantined: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "corrupt ledger was not preserved");
        assert!(std::fs::read_to_string(quarantined[0].path())
            .unwrap()
            .contains("keep-me"));

        // The next write starts clean instead of silently destroying it.
        mark_harvested(dir, "new", &[], HarvestDecision::Skipped, None).unwrap();
        let after = read_harvested(dir);
        assert!(after.contains_key("new") && !after.contains_key("keep-me"));
    }

    #[test]
    fn a_forward_incompatible_entry_does_not_destroy_the_whole_ledger() {
        // Deserialization is all-or-nothing, so one unparseable field from a
        // future version discards every entry — the same data-loss path.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None).unwrap();
        let path = ledger_path(dir);
        std::fs::write(&path, r#"{"s1":{"decision":"from_the_future_v9"}}"#).unwrap();

        assert!(read_harvested(dir).is_empty());
        assert!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains(".corrupt-")),
            "a forward-incompat ledger was discarded without a copy"
        );
    }
}
