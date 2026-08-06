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
    /// Nobody has looked at it yet. Written by [`set_archive`] for a session
    /// the SessionEnd hook archived, which is every session that ever ended.
    ///
    /// Separate from [`Self::Deferred`] because the two carry opposite
    /// information and only one of them is a statement about the content: a
    /// deferral means a human read the session and postponed the call, while
    /// this means the machine noticed the session stop. Sharing a variant made
    /// `harvest ledger list` report `Deferred` for the entire history of the
    /// machine, drowning the handful of real deferrals in it.
    Unreviewed,
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
    /// offering it. Neither `Deferred` nor `Unreviewed` does.
    pub fn is_settled(&self) -> bool {
        !matches!(
            self.decision(),
            HarvestDecision::Deferred | HarvestDecision::Unreviewed
        )
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
    read_ledger_at(&ledger_path(project_dir))
}

/// [`read_harvested`] against an explicit file, so [`adopt_ledger`] can read a
/// ledger it has already moved aside.
fn read_ledger_at(path: &Path) -> HashMap<String, HarvestEntry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    match serde_json::from_str(&raw) {
        Ok(map) => map,
        Err(e) => {
            let quarantine = path.with_extension(format!("json.corrupt-{}", std::process::id()));
            let moved = std::fs::rename(path, &quarantine).is_ok();
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
/// a session nobody has reviewed yet) is `Unreviewed` and deliberately does
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
                    decision: Some(HarvestDecision::Unreviewed),
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
/// The exemption is written against the entry's *claim*, which is all this
/// function can see: it takes the ledger directory, and locating the file
/// needs the root project id. What keeps the claim honest — and the file
/// bounded — is [`reconcile_archive_refs`], which every harvest entry point
/// runs against the archive directory before touching the ledger.
fn prune_stale(map: &mut HashMap<String, HarvestEntry>) {
    let cutoff = Utc::now() - Duration::days(PRUNE_AFTER_DAYS);
    map.retain(|_, e| e.archive.is_some() || e.harvested_at > cutoff);
}

/// Drop archive references whose file is no longer on disk, returning the
/// session ids that lost one.
///
/// [`prune_stale`] exempts an entry that names an archive, and `prune_archives`
/// — the only other thing that calls [`clear_archive_refs`] — reports what a
/// *directory scan* found, so it can never report a file that is already gone.
/// A reference the file never caught up with therefore made its entry immortal:
/// exempt from the age window forever, advertising an export that cannot
/// succeed. `projects delete --cascade` removes the archive directory without
/// touching the ledger, archives written before a `projects link` sit under the
/// child's old id where the root's prune never looks, and eviction on another
/// machine, a restored backup or a manual cleanup do the same.
///
/// Reconciling against the directory rather than stat-ing each file keeps this
/// one `read_dir`, and makes an unreadable-but-present directory an error
/// (nothing is cleared) instead of a mass deletion of references. A file that
/// exists under some *other* project's id is still cleared: every route from a
/// session id to a transcript resolves through the root project id, so such a
/// file is already unreachable, and pretending otherwise is what the entry's
/// immortality was built on.
pub fn reconcile_archive_refs(project_dir: &Path, project_id: &str) -> Result<Vec<String>> {
    let claimed: Vec<String> = read_harvested(project_dir)
        .into_iter()
        .filter(|(_, e)| e.archive.is_some())
        .map(|(id, _)| id)
        .collect();
    if claimed.is_empty() {
        return Ok(Vec::new());
    }
    let present: std::collections::HashSet<String> =
        crate::transcript_archive::list_archives(project_id)?
            .into_iter()
            .map(|a| a.session_id)
            .collect();
    let dangling: Vec<String> = claimed
        .into_iter()
        .filter(|id| !present.contains(id))
        .collect();
    clear_archive_refs(project_dir, &dangling)?;
    Ok(dangling)
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
///
/// **Adoption is all-or-nothing, and the move aside is the step that commits
/// it.** Merging first and moving after meant a failed rename was swallowed
/// with the root already written: every later resolve re-merged the same
/// sub-ledger, so a `harvest reset` was undone the moment the next harvest
/// command ran and no amount of clearing could settle the session. Renaming
/// first inverts that — a rename that fails adopts nothing, which the caller
/// already treats as advisory ("the old decisions, not the harvest"), and a
/// process that dies between the two steps leaves the data intact under the
/// `.adopted` name rather than losing it.
pub fn adopt_ledger(sub_dir: &Path, root_dir: &Path) -> Result<()> {
    let legacy_path = ledger_path(sub_dir);
    if !legacy_path.exists() {
        return Ok(());
    }
    // Compare the resolved *files*, not the spellings. A textual `==` looks
    // sufficient but is not: the shipped MCP entry is `serve --dir .`, so the
    // invoking dir is literally `"."` while the registry holds the canonical
    // absolute path — the same directory under two names. This function then
    // took the ledger's `flock` for the sub, renamed the live ledger to
    // `.adopted`, and took the same lock again for the root on the *same
    // inode*, blocking forever with the ledger already moved aside. That hung
    // the SessionEnd hook and pinned a worker (and the lock) in `serve`.
    // Any non-canonical spelling does it: `.`, a relative `--dir`, a symlinked
    // checkout, or `/tmp` -> `/private/tmp` on macOS.
    let same_file = match (
        std::fs::canonicalize(&legacy_path),
        std::fs::canonicalize(ledger_path(root_dir)),
    ) {
        (Ok(a), Ok(b)) => a == b,
        // The root ledger may legitimately not exist yet; fall back to
        // comparing the directories, still canonically where possible.
        _ => match (
            std::fs::canonicalize(sub_dir),
            std::fs::canonicalize(root_dir),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => sub_dir == root_dir,
        },
    };
    if same_file {
        return Ok(());
    }
    // Lock the *source* too, and hold it across the rename. Locking only the
    // destination left the read and the move aside unguarded, so a concurrent
    // `mark_harvested(sub_dir, …)` landing between them was renamed away
    // without ever being merged — a review decision lost with no trace. A
    // writer blocked here instead resumes against a missing file and recreates
    // it holding just its own entry, which the next adoption picks up.
    let _sub_lock = lock_ledger(sub_dir);
    let adopted_path = legacy_path.with_extension("json.adopted");
    std::fs::rename(&legacy_path, &adopted_path)?;

    // Reads as empty (and quarantines the file) if it is corrupt, in which
    // case there is nothing to adopt.
    let legacy = read_ledger_at(&adopted_path);
    if legacy.is_empty() {
        return Ok(());
    }
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
    write_harvested(root_dir, &map)
}

/// Did a person record this decision, or did the machine?
///
/// The only axis on which [`merge_entries`] overrides recency. `Deferred` sits
/// on the human side despite being unsettled: postponing a call is a call.
fn is_human_decision(decision: HarvestDecision) -> bool {
    !matches!(decision, HarvestDecision::Unreviewed)
}
/// Reconcile two records of the same session found in two ledgers.
///
/// A human decision outranks `Unreviewed` regardless of timestamps: the
/// SessionEnd hook writes one for every session it archives, so the root's copy
/// is routinely *younger* than the review it must not overwrite. Between two
/// human decisions the newer wins — including a `Deferred` that supersedes an
/// older `Harvested`/`Skipped`, which is the whole point of asking for another
/// look. The rule was once "settled beats unsettled", grouping `Deferred` with
/// the hook's entries, and that made a deliberate `--defer` on the root lose
/// deterministically to whatever the sub-project had recorded first.
///
/// An archive reference is kept from whichever side has one — the file it names
/// is the same either way, since archives have always been keyed by the root
/// project.
fn merge_entries(root: &HarvestEntry, legacy: &HarvestEntry) -> HarvestEntry {
    let prefer_legacy = match (
        is_human_decision(root.decision()),
        is_human_decision(legacy.decision()),
    ) {
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

/// What [`clear_harvested`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearOutcome {
    /// No entry for this session; nothing changed.
    NotFound,
    /// The entry held no archive and was dropped whole.
    Removed,
    /// The review was erased but the entry kept, as `Unreviewed`, because it
    /// is the only route to an archived transcript.
    ResetToUnreviewed,
}

impl ClearOutcome {
    /// Did the ledger change?
    pub fn changed(self) -> bool {
        !matches!(self, ClearOutcome::NotFound)
    }

    /// Is an archived transcript still reachable through this session?
    pub fn kept_archive(self) -> bool {
        matches!(self, ClearOutcome::ResetToUnreviewed)
    }
}

/// Forget a session's review so it is offered again.
///
/// **Keeps the entry when it names an archive**, resetting it to `Unreviewed`
/// instead of removing it. An archive is reachable only *through* its ledger
/// entry — `harvest show`, `ledger export` and `ledger rm` all resolve a
/// session id against this map first — so dropping the entry left the `.zst`
/// on disk with no route to it from any command, still counting against the
/// archive budget, while the caller was told the session "will be offered
/// again". For a session whose live transcript Claude Code has already pruned
/// that was false twice over: nothing offered it, and the one surviving copy
/// had just been orphaned. This is the same reasoning [`prune_stale`] applies
/// to the age window.
///
/// `Unreviewed` (not deleted, not `Deferred`) because it is exactly the state
/// the SessionEnd hook would have left behind: archived, unsettled, and
/// carrying no claim that a human looked at it.
pub fn clear_harvested(project_dir: &Path, session_id: &str) -> Result<ClearOutcome> {
    if session_id.is_empty() {
        return Ok(ClearOutcome::NotFound);
    }
    let _lock = lock_ledger(project_dir);
    let mut map = read_harvested(project_dir);
    let Some(existing) = map.remove(session_id) else {
        return Ok(ClearOutcome::NotFound);
    };
    let outcome = match existing.archive {
        Some(archive) => {
            map.insert(
                session_id.to_string(),
                HarvestEntry {
                    harvested_at: Utc::now(),
                    memories_created: 0,
                    memory_ids: Vec::new(),
                    decision: Some(HarvestDecision::Unreviewed),
                    note: None,
                    archive: Some(archive),
                },
            );
            ClearOutcome::ResetToUnreviewed
        }
        None => ClearOutcome::Removed,
    };
    write_harvested(project_dir, &map)?;
    Ok(outcome)
}

fn write_harvested(project_dir: &Path, map: &HashMap<String, HarvestEntry>) -> Result<()> {
    let json = serde_json::to_string_pretty(map)
        .map_err(|e| crate::error::StorageError::Validation(e.to_string()))?;
    // Atomic temp-then-rename, same discipline as memory-file writes — and
    // symlink-refusing, because the temp path is predictable and `.engramdb/`
    // is committed. See `crate::state_file`.
    crate::state_file::write_state_json(&ledger_path(project_dir), &json)
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

        assert_eq!(clear_harvested(dir, "s1").unwrap(), ClearOutcome::Removed);
        assert!(!is_harvested(dir, "s1"));
        assert_eq!(clear_harvested(dir, "s1").unwrap(), ClearOutcome::NotFound);
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
        assert_eq!(map["s1"].decision(), HarvestDecision::Unreviewed);
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

    #[test]
    fn a_dangling_archive_reference_stops_exempting_its_entry() {
        // The age exemption was written against the entry's *claim*, and
        // nothing ever reconciled the claim with the disk: `prune_archives`
        // reports only files a directory scan found, so a reference whose file
        // went another way — `projects delete --cascade`, an archive written
        // under the pre-`projects link` id, eviction on another machine —
        // never reached `clear_archive_refs` and made its entry immortal.
        let tmp = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let prev = std::env::var("ENGRAMDB_DATA_DIR").ok();
        std::env::set_var("ENGRAMDB_DATA_DIR", data.path());

        let dir = tmp.path();
        let long_ago = Utc::now() - Duration::days(PRUNE_AFTER_DAYS + 35);
        let mut seeded = HashMap::new();
        seeded.insert(
            "ghost".to_string(),
            HarvestEntry {
                harvested_at: long_ago,
                memories_created: 0,
                memory_ids: vec![],
                decision: Some(HarvestDecision::Skipped),
                note: None,
                archive: Some(sample_archive()),
            },
        );
        write_harvested(dir, &seeded).unwrap();

        // No archive directory at all: every claim is dangling.
        let cleared = reconcile_archive_refs(dir, "proj").unwrap();
        assert_eq!(cleared, vec!["ghost"], "the dangling ref was not noticed");
        assert!(
            !read_harvested(dir).contains_key("ghost"),
            "an entry naming a file that does not exist outlived the age window"
        );

        match prev {
            Some(v) => std::env::set_var("ENGRAMDB_DATA_DIR", v),
            None => std::env::remove_var("ENGRAMDB_DATA_DIR"),
        }
    }

    #[test]
    fn reconciling_keeps_references_whose_file_is_really_there() {
        // Negative control: the whole point of the exemption is that a live
        // archive keeps its only index into itself.
        let tmp = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let prev = std::env::var("ENGRAMDB_DATA_DIR").ok();
        std::env::set_var("ENGRAMDB_DATA_DIR", data.path());

        let src = TempDir::new().unwrap();
        let transcript = src.path().join("live.jsonl");
        std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
        let archive = crate::transcript_archive::archive_transcript("proj", "live", &transcript)
            .expect("archive written");
        set_archive(tmp.path(), "live", archive).unwrap();

        assert!(reconcile_archive_refs(tmp.path(), "proj")
            .unwrap()
            .is_empty());
        assert!(read_harvested(tmp.path())["live"].archive.is_some());

        match prev {
            Some(v) => std::env::set_var("ENGRAMDB_DATA_DIR", v),
            None => std::env::remove_var("ENGRAMDB_DATA_DIR"),
        }
    }

    #[test]
    fn a_failed_move_aside_adopts_nothing() {
        // The rename used to run after the merge had been committed, and its
        // failure was swallowed — so the sub-ledger stayed put and every later
        // resolve re-merged it, resurrecting whatever `harvest reset` had just
        // cleared. Adoption has to be all-or-nothing.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("child");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        mark_harvested(&sub, "s1", &[], HarvestDecision::Skipped, None).unwrap();
        // Block the rename by occupying its target with a directory — the
        // stand-in for the real case, a sub-project the user cannot write to.
        std::fs::create_dir_all(ledger_path(&sub).with_extension("json.adopted")).unwrap();

        assert!(
            adopt_ledger(&sub, &root).is_err(),
            "a failed move aside must be reported, not swallowed"
        );
        assert!(
            !read_harvested(&root).contains_key("s1"),
            "the merge was committed even though the sub-ledger could not be retired"
        );

        // The consequence that made it permanent: clearing the entry must stick.
        mark_harvested(&root, "s1", &[], HarvestDecision::Skipped, None).unwrap();
        assert!(clear_harvested(&root, "s1").unwrap().changed());
        let _ = adopt_ledger(&sub, &root);
        assert!(
            !is_harvested(&root, "s1"),
            "a reset session came back from the un-retired sub-ledger"
        );
    }

    #[test]
    fn adoption_holds_the_sub_ledger_lock_while_it_moves_it() {
        // The snapshot and the rename were unguarded, so a concurrent
        // `mark_harvested(sub_dir, …)` between them was moved aside unmerged.
        // Holding the source lock is what closes that window; assert the lock
        // is actually taken, since the race itself is not reproducible.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("child");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        mark_harvested(&sub, "s1", &[], HarvestDecision::Skipped, None).unwrap();

        // Hold the source lock, then adopt from another thread: it must block
        // rather than sail past the unguarded read-and-rename.
        let held = lock_ledger(&sub).expect("lock the sub-ledger");
        let (sub_c, root_c) = (sub.clone(), root.clone());
        let handle = std::thread::spawn(move || adopt_ledger(&sub_c, &root_c));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !handle.is_finished(),
            "adoption ran through while the sub-ledger was locked"
        );
        drop(held);
        handle.join().unwrap().unwrap();
        assert!(read_harvested(&root).contains_key("s1"));
    }

    #[test]
    fn an_unreviewed_archive_entry_is_not_a_deliberate_deferral() {
        // The hook archives every session that ever ends, so sharing the
        // `Deferred` variant made `harvest ledger list` report a deliberate
        // postponement for the machine's entire history.
        let tmp = TempDir::new().unwrap();
        set_archive(tmp.path(), "auto", sample_archive()).unwrap();
        mark_harvested(tmp.path(), "chosen", &[], HarvestDecision::Deferred, None).unwrap();

        let map = read_harvested(tmp.path());
        assert_eq!(map["auto"].decision(), HarvestDecision::Unreviewed);
        assert_eq!(map["chosen"].decision(), HarvestDecision::Deferred);
        // Both still keep being offered — that part must not change.
        assert!(!map["auto"].is_settled() && !map["chosen"].is_settled());
    }

    #[test]
    fn clearing_a_review_does_not_strand_its_archive() {
        // Removing the entry left the `.zst` on disk with no route to it:
        // `harvest show`, `ledger export` and `ledger list` all resolve a
        // session id against this map, so the last surviving copy of a
        // transcript Claude Code had already pruned became unreachable — while
        // the caller was told the session would be offered again.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        set_archive(dir, "s1", sample_archive()).unwrap();
        mark_harvested(dir, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();

        let outcome = clear_harvested(dir, "s1").unwrap();
        assert_eq!(outcome, ClearOutcome::ResetToUnreviewed);
        assert!(outcome.kept_archive() && outcome.changed());

        let entry = read_harvested(dir)
            .get("s1")
            .cloned()
            .expect("the only index into the archive was deleted");
        assert!(entry.archive.is_some(), "the archive reference was dropped");
        assert_eq!(entry.decision(), HarvestDecision::Unreviewed);
        assert!(entry.memory_ids.is_empty(), "the review must be erased");
        assert_eq!(entry.memories_created, 0);
        assert!(
            !is_harvested(dir, "s1"),
            "the session must still be offered"
        );
    }

    #[test]
    fn clearing_a_review_with_no_archive_still_removes_the_entry() {
        // The other half of the contract the success message rests on: with no
        // file behind it there is nothing to strand, so the entry goes.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None).unwrap();

        let outcome = clear_harvested(dir, "s1").unwrap();
        assert_eq!(outcome, ClearOutcome::Removed);
        assert!(!outcome.kept_archive());
        assert!(!read_harvested(dir).contains_key("s1"));
    }

    #[test]
    fn merge_precedence_covers_every_decision_pair() {
        // Two rules, exhaustively: `Unreviewed` never beats a human decision,
        // and between two human decisions recency wins. The second is what a
        // later `--defer` on the root depends on — under the old
        // "settled beats unsettled" rule it lost to any older `Harvested` or
        // `Skipped` from the sub-project, deterministically and silently.
        use HarvestDecision::*;
        let older = Utc::now() - Duration::days(2);
        let newer = Utc::now();
        let at = |decision: HarvestDecision, when: DateTime<Utc>| HarvestEntry {
            harvested_at: when,
            memories_created: 0,
            memory_ids: vec![],
            decision: Some(decision),
            note: None,
            archive: None,
        };

        // (root, legacy, expected) — every ordered pair of the four decisions,
        // with the *root* side older so recency and human-weight disagree
        // wherever they can.
        let cases = [
            (Harvested, Skipped, Skipped),
            (Harvested, Deferred, Deferred),
            (Harvested, Unreviewed, Harvested),
            (Skipped, Harvested, Harvested),
            (Skipped, Deferred, Deferred),
            (Skipped, Unreviewed, Skipped),
            (Deferred, Harvested, Harvested),
            (Deferred, Skipped, Skipped),
            (Deferred, Unreviewed, Deferred),
            (Unreviewed, Harvested, Harvested),
            (Unreviewed, Skipped, Skipped),
            (Unreviewed, Deferred, Deferred),
            // Like against like: recency alone.
            (Harvested, Harvested, Harvested),
            (Skipped, Skipped, Skipped),
            (Deferred, Deferred, Deferred),
            (Unreviewed, Unreviewed, Unreviewed),
        ];
        for (root, legacy, expected) in cases {
            let merged = merge_entries(&at(root, older), &at(legacy, newer));
            assert_eq!(
                merged.decision(),
                expected,
                "older root {root:?} + newer legacy {legacy:?}"
            );
            // Mirrored: swapping which side is newer swaps the winner unless
            // human weight decides it.
            let mirrored = merge_entries(&at(root, newer), &at(legacy, older));
            let expected_mirror = match (is_human_decision(root), is_human_decision(legacy)) {
                (false, true) => legacy,
                _ => root,
            };
            assert_eq!(
                mirrored.decision(),
                expected_mirror,
                "newer root {root:?} + older legacy {legacy:?}"
            );
        }
    }

    #[test]
    fn adoption_keeps_a_later_deferral_over_an_older_settled_decision() {
        // The user reviewed the session on the sub-project, then asked for
        // another look from the root. Adoption must not restore the decision
        // the deferral was meant to reopen.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("child");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        mark_harvested(&sub, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();
        mark_harvested(
            &root,
            "s1",
            &[],
            HarvestDecision::Deferred,
            Some("revisit after the refactor".into()),
        )
        .unwrap();

        adopt_ledger(&sub, &root).unwrap();

        let entry = &read_harvested(&root)["s1"];
        assert_eq!(
            entry.decision(),
            HarvestDecision::Deferred,
            "a deliberate deferral was overwritten by an older settled decision"
        );
        assert!(!is_harvested(&root, "s1"), "the session must be re-offered");
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
        assert!(!clear_harvested(tmp.path(), "").unwrap().changed());
    }

    /// A symlink committed at the ledger's temp path arrives in every clone
    /// (`.engramdb/` is tracked), and the SessionEnd hook writes here
    /// unattended — so following one is an arbitrary-file overwrite that
    /// needs no local access at all.
    #[test]
    #[cfg(unix)]
    fn a_planted_temp_symlink_cannot_redirect_a_mark() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();

        let path = ledger_path(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("json.tmp")).unwrap();

        let result = mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None);
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the ledger write landed on the symlink's target"
        );
        assert!(result.is_err(), "a redirected write reported success");
        assert!(!path.exists(), "the symlink was renamed onto the ledger");
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
