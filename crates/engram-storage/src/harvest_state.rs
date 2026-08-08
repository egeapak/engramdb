//! Append-only log of what has happened to each Claude Code session.
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
//! # Why a log and not a map
//!
//! This was a whole-map JSON file rewritten under an advisory `flock(2)`. Every
//! writer read the map, changed one key and wrote the map back, so two writers
//! had to be serialized or one of them silently lost an entry — and the
//! SessionEnd hook, which fires unattended on every session that ever ends, was
//! one of those writers. Four defects on this branch came out of that machinery
//! rather than out of the feature.
//!
//! A write here is an **append of one line**, never a read-modify-write. That
//! makes the hook cheap (one `open`+`write`), makes concurrent writers safe
//! without a lock (`O_APPEND` places each record at the end atomically), and
//! makes a partial write cost exactly the torn line instead of the file — so
//! the lock, the four-state merge precedence and the corrupt-file quarantine
//! are all gone with the map they existed for.
//!
//! # Line format
//!
//! One JSON object per line, at `.engramdb/state/harvest_ledger.jsonl`:
//!
//! ```text
//! {"session_id":"aaaa1111","at":"2026-08-07T10:11:12.131415Z","stage":"collected","archive":{"file_name":"aaaa1111.jsonl.zst","bytes":143,"original_bytes":901,"sha256":"deadbeef","archived_at":"2026-08-07T10:11:12.131400Z"}}
//! {"session_id":"aaaa1111","at":"2026-08-07T11:02:00.500Z","decision":"harvested","memory_ids":["m1","m2"],"note":null}
//! ```
//!
//! **A line is a patch, not a snapshot**: a field that is absent means "this
//! transition did not touch it". That is what lets the two writers that share a
//! session — the SessionEnd hook writing where the bytes are, and the harvest
//! flow writing what a human concluded — each append blindly without reading,
//! and without either clobbering the other. `null` is distinct from absent and
//! means "clear this field", which is how an archive reference is dropped.
//!
//! Lines are folded **in timestamp order**, ties broken by file order. Position
//! is deliberately *not* the ordering: [`adopt_ledger`] concatenates a whole
//! foreign log onto the end of this one, and compaction rewrites the file
//! underneath concurrent appenders. Both are correct only if a record carries
//! its own place in the sequence. The cost is that a clock stepped backwards
//! could let an older record win; on a single machine's ledger that is a far
//! smaller exposure than the two reorderings it buys.
//!
//! # Two axes, not one
//!
//! [`HarvestStage`] (mechanical — where the bytes are) and [`HarvestDecision`]
//! (human — what was concluded) are independent and must not be collapsed. A
//! session can be `compressed` while `deferred`, or `indexed` while `skipped`.
//!
//! Entries are keyed by session id and live under the **root** project, so a
//! worktree's sessions and the main checkout's sessions share one log —
//! matching the fact that they share one memory store. Every function here
//! takes the directory to use verbatim; resolving the root is the caller's
//! job, and [`adopt_ledger`] is what folds a log an older version left at a
//! non-root path into the root's.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Entries older than this are dropped, bounding the log's growth on a
/// long-lived project. Well past any window in which re-harvesting an old
/// session would be useful.
///
/// Applies only to entries that hold no archive — see [`drain_and_prune`].
const PRUNE_AFTER_DAYS: i64 = 365;

/// Compact once the file holds more than this many lines per live entry.
///
/// Compaction leaves exactly one line per entry, so this is the factor by
/// which the log may exceed the information it carries. Four is the steady
/// state of one session's whole life — collect, decide, index, drop the
/// archive reference — so a file at 4x is one whose excess is *dead* entries
/// (drained, aged out, tombstoned) rather than live history. It also amortizes:
/// each rewrite is paid for by at least three appends per entry since the last
/// one.
const COMPACT_FACTOR: usize = 4;

/// Don't even look at compacting below this size.
///
/// Counting lines means reading the file, which an append otherwise never does
/// — the whole point of the format. This gate keeps the common case at one
/// `stat` plus one `write`. At the ~200 bytes a line runs to, it is around 300
/// lines, i.e. roughly 80 live entries at [`COMPACT_FACTOR`].
const COMPACT_MIN_BYTES: u64 = 64 * 1024;

/// How many times compaction re-reads the tail before giving up.
///
/// Compaction is advisory: when it cannot prove it captured every byte, it
/// leaves the file alone rather than risk dropping a record.
const COMPACT_TAIL_ATTEMPTS: usize = 4;

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
    /// Nobody has looked at it yet — the state of every entry no `decision`
    /// line has ever been written for, which is every session the SessionEnd
    /// hook collected and nobody reviewed.
    ///
    /// Separate from [`Self::Deferred`] because the two carry opposite
    /// information and only one of them is a statement about the content: a
    /// deferral means a human read the session and postponed the call, while
    /// this means the machine noticed the session stop. Sharing a variant made
    /// `harvest ledger list` report `Deferred` for the entire history of the
    /// machine, drowning the handful of real deferrals in it.
    Unreviewed,
}

/// Where the session's bytes are.
///
/// The mechanical axis, independent of [`HarvestDecision`]: `collected` is the
/// verbatim transcript copy the SessionEnd hook takes, `indexed` adds a
/// searchable row, `compressed` is the encoded end state. An entry that reaches
/// `compressed` leaves this log — the index row becomes the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarvestStage {
    /// The conversation is reachable as raw bytes — a live transcript, or the
    /// copy the SessionEnd hook took. The baseline for any entry that has
    /// never been staged further.
    Collected,
    /// A search row exists for it.
    Indexed,
    /// Encoded end state. Entries do not stay here; see
    /// [`drain_and_prune`].
    Compressed,
}

/// The state of one session, folded from every line written about it.
#[derive(Debug, Clone)]
pub struct HarvestEntry {
    /// When the session's *decision* was last recorded. Lines that carry no
    /// decision — a collect, an archive reference being dropped — deliberately
    /// leave it alone, so archiving a session does not make its review look
    /// fresh.
    pub harvested_at: DateTime<Utc>,
    /// How many memories the user accepted from it. `0` is a meaningful,
    /// deliberately-recorded value — see the module docs.
    pub memories_created: usize,
    /// Ids of the memories created from this session, for later attribution.
    pub memory_ids: Vec<String>,
    /// The recorded decision, or `None` when no line has ever set one.
    ///
    /// [`Self::decision`] reads that as [`HarvestDecision::Unreviewed`]. The
    /// old "infer it from `memories_created`" rule now runs once, at migration
    /// time, so every entry carried over from the JSON map arrives with an
    /// explicit decision and `None` has exactly one meaning.
    pub decision: Option<HarvestDecision>,
    /// Where the bytes are.
    pub stage: HarvestStage,
    /// Free-text note, e.g. why a session was skipped.
    pub note: Option<String>,
    /// The archived transcript, when one is held.
    pub archive: Option<crate::transcript_archive::ArchiveRef>,
}

impl HarvestEntry {
    /// A session nothing has been said about yet, as of `at`.
    fn seed(at: DateTime<Utc>) -> Self {
        Self {
            harvested_at: at,
            memories_created: 0,
            memory_ids: Vec::new(),
            decision: None,
            stage: HarvestStage::Collected,
            note: None,
            archive: None,
        }
    }

    /// The decision, reading "never recorded" as `Unreviewed`.
    pub fn decision(&self) -> HarvestDecision {
        self.decision.unwrap_or(HarvestDecision::Unreviewed)
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

/// One appended record: the fields this transition changed.
///
/// Every optional field distinguishes three states — absent (unchanged), a
/// value, and (where the field is itself optional) `null` (cleared). That is
/// the whole reason a blind append is safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerLine {
    session_id: String,
    at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stage: Option<HarvestStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<HarvestDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory_ids: Option<Vec<String>>,
    /// Only written by the migration, which carries over counts from ledgers
    /// that recorded a total without the ids behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memories_created: Option<usize>,
    #[serde(
        default,
        deserialize_with = "explicit_option",
        skip_serializing_if = "Option::is_none"
    )]
    note: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "explicit_option",
        skip_serializing_if = "Option::is_none"
    )]
    archive: Option<Option<crate::transcript_archive::ArchiveRef>>,
    /// Tombstone: the entry is gone as of `at`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    removed: bool,
}

impl LedgerLine {
    /// A patch that changes nothing, to be filled in by the caller.
    fn touching(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            at: Utc::now(),
            stage: None,
            decision: None,
            memory_ids: None,
            memories_created: None,
            note: None,
            archive: None,
            removed: false,
        }
    }
}

/// Deserialize `Option<Option<T>>` so a present `null` is `Some(None)`.
///
/// Serde's derived impl collapses a present `null` to `None`, which is exactly
/// the distinction this format depends on: absent means "unchanged", `null`
/// means "cleared". Without this, dropping an archive reference would be a
/// no-op line.
fn explicit_option<'de, D, T>(de: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// A folded entry plus the newest timestamp any of its lines carried.
///
/// Compaction needs the latter: a snapshot stamped with `harvested_at` (the
/// last *decision*) would sort ahead of a later collect it already subsumes,
/// and a concurrent append in between would then win against data newer than
/// itself.
#[derive(Debug, Clone)]
struct Folded {
    entry: HarvestEntry,
    last_at: DateTime<Utc>,
}

/// Where the log lives under a project root.
fn ledger_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("harvest_ledger.jsonl")
}

/// The whole-map JSON file this format replaced. Read once, then retired.
fn legacy_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".engramdb")
        .join("state")
        .join("harvested_sessions.json")
}

/// Read the log. A missing file reads as empty.
pub fn read_harvested(project_dir: &Path) -> HashMap<String, HarvestEntry> {
    read_folded(project_dir)
        .into_iter()
        .map(|(id, f)| (id, f.entry))
        .collect()
}

/// The fold, with the per-entry high-water timestamp compaction needs.
fn read_folded(project_dir: &Path) -> HashMap<String, Folded> {
    migrate_legacy_map(project_dir);
    let raw = std::fs::read_to_string(ledger_path(project_dir)).unwrap_or_default();
    let mut folded = fold(parse_lines(&raw));
    drain_and_prune(&mut folded);
    folded
}

/// Parse every line, in timestamp order, skipping the ones that cannot be read.
///
/// A line is skipped rather than fatal, and the rest of the log stands. This is
/// what replaces the quarantine the whole-map format needed: a torn final line
/// (the shape a crash mid-append leaves) or a record from a future version
/// costs one entry, not every review decision in the file. It is still
/// **reported** — a silently dropped record is indistinguishable from a session
/// nobody ever marked.
fn parse_lines(raw: &str) -> Vec<LedgerLine> {
    let mut out: Vec<LedgerLine> = Vec::new();
    for (n, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<LedgerLine>(line) {
            // A session id is later joined into an archive path by
            // `harvest ledger rm` / `export`. The writers validate, but a line
            // can also arrive by concatenation from another project's log.
            Ok(parsed) if crate::transcripts::is_valid_session_id(&parsed.session_id) => {
                out.push(parsed)
            }
            Ok(parsed) => tracing::warn!(
                "harvest ledger: skipping line {} — {:?} is not a plain session id",
                n + 1,
                parsed.session_id
            ),
            Err(e) => tracing::warn!(
                "harvest ledger: skipping unreadable line {} ({e}); every other line still counts",
                n + 1
            ),
        }
    }
    // Stable, so same-instant lines keep the order they were written in.
    out.sort_by_key(|l| l.at);
    out
}

/// Apply the patches in order. Later lines win, field by field.
fn fold(lines: Vec<LedgerLine>) -> HashMap<String, Folded> {
    let mut map: HashMap<String, Folded> = HashMap::new();
    for line in lines {
        if line.removed {
            map.remove(&line.session_id);
            continue;
        }
        let folded = map.entry(line.session_id).or_insert_with(|| Folded {
            entry: HarvestEntry::seed(line.at),
            last_at: line.at,
        });
        folded.last_at = folded.last_at.max(line.at);
        if let Some(stage) = line.stage {
            folded.entry.stage = stage;
        }
        if let Some(decision) = line.decision {
            folded.entry.decision = Some(decision);
            folded.entry.harvested_at = line.at;
        }
        if let Some(ids) = line.memory_ids {
            folded.entry.memories_created = ids.len();
            folded.entry.memory_ids = ids;
        }
        // After `memory_ids`, so a migrated count with no ids behind it stands.
        if let Some(count) = line.memories_created {
            folded.entry.memories_created = count;
        }
        if let Some(note) = line.note {
            folded.entry.note = note;
        }
        if let Some(archive) = line.archive {
            folded.entry.archive = archive;
        }
    }
    map
}

/// Drop entries that have left the log, loudly, and ones past the age window.
///
/// **The drain.** An entry that reaches [`HarvestStage::Compressed`] is meant to
/// be carried by an index row from then on. That row does not exist yet in this
/// version, so draining one really does destroy its review record — which is
/// why it is a `warn` naming the sessions rather than a quiet `retain`. Nothing
/// in this version *writes* `compressed`; the path exists so the format does
/// not have to change under the index.
///
/// **The age window** drops entries past [`PRUNE_AFTER_DAYS`], **except** ones
/// that still point at an archive. An archive is only reachable *through* its
/// entry: `harvest show`, `ledger export` and `ledger rm` all resolve a session
/// id against this map first, so dropping an entry whose file is still on disk
/// makes that file unreachable by every route while it keeps consuming the
/// archive budget. The two windows are independent and users are explicitly
/// told to set `archive_retention_days = 3650`, so "the archive pass will
/// reclaim it" holds only when that window is the shorter of the two — which is
/// exactly the configuration the docs steer people away from.
///
/// The exemption is written against the entry's *claim*, which is all this
/// function can see: it takes the ledger directory, and locating the file needs
/// the root project id. What keeps the claim honest — and the file bounded — is
/// [`reconcile_archive_refs`], which every harvest entry point runs against the
/// archive directory before touching the log.
///
/// Both are applied at *read*, not at write: an append cannot afford to know
/// what else is in the file, and compaction is what actually reclaims the
/// bytes.
fn drain_and_prune(folded: &mut HashMap<String, Folded>) {
    let mut drained: Vec<String> = folded
        .iter()
        .filter(|(_, f)| f.entry.stage == HarvestStage::Compressed)
        .map(|(id, _)| id.clone())
        .collect();
    if !drained.is_empty() {
        drained.sort();
        tracing::warn!(
            "harvest ledger: dropping {} entr{} that reached the `compressed` stage ({}); \
             no index row carries them in this version, so their review records are gone",
            drained.len(),
            if drained.len() == 1 { "y" } else { "ies" },
            drained.join(", ")
        );
        for id in &drained {
            folded.remove(id);
        }
    }
    let cutoff = Utc::now() - Duration::days(PRUNE_AFTER_DAYS);
    folded.retain(|_, f| f.entry.archive.is_some() || f.entry.harvested_at > cutoff);
}

/// How far the log has outgrown the state it carries: `(lines, live entries)`.
///
/// The same two numbers [`maybe_compact`] compares, exposed so `doctor` can
/// report pending compaction without duplicating the fold. A missing file is
/// `(0, 0)`.
///
/// Reported rather than acted on: compaction is opportunistic (it happens on
/// the next append that crosses the size gate) and a log that is merely long
/// costs disk, not correctness. What a user needs to know is that the append
/// path has not run recently enough to reclaim it — a project nobody has
/// harvested in months never appends, so the file sits at whatever size it
/// reached.
pub fn compaction_pressure(project_dir: &Path) -> (usize, usize) {
    let Ok(raw) = std::fs::read_to_string(ledger_path(project_dir)) else {
        return (0, 0);
    };
    let lines = raw.lines().filter(|l| !l.trim().is_empty()).count();
    let mut folded = fold(parse_lines(&raw));
    drain_and_prune(&mut folded);
    (lines, folded.len())
}

/// Whether the log has outgrown its live entries by the factor compaction acts
/// on. Shares [`COMPACT_FACTOR`] with [`maybe_compact`] so the two can never
/// disagree about what "pending" means.
pub fn compaction_is_pending(lines: usize, live: usize) -> bool {
    lines > live.max(1) * COMPACT_FACTOR
}

/// Is this session settled — i.e. should `list` stop offering it?
///
/// An entry that only records a collected transcript (written by the SessionEnd
/// hook for a session nobody has reviewed yet) is `Unreviewed` and deliberately
/// does **not** count: archiving a transcript must never make it invisible to
/// the very command that exists to review it.
pub fn is_harvested(project_dir: &Path, session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    read_folded(project_dir)
        .get(session_id)
        .is_some_and(|f| f.entry.is_settled())
}

/// Record a review decision for `session_id`.
///
/// The line carries only the decision axis, so an archive reference or a stage
/// written by the SessionEnd hook — before or after this call — is untouched.
/// That is what the old read-modify-write needed a lock and a merge function
/// to achieve.
///
/// Returns the session's state *after* the append, folded from the file rather
/// than synthesized from the patch, so the caller sees the archive it did not
/// write.
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
    let mut line = LedgerLine::touching(session_id);
    line.decision = Some(decision);
    line.memory_ids = Some(memory_ids.to_vec());
    line.note = Some(note.clone());
    let at = line.at;
    append(project_dir, &[line])?;

    Ok(read_folded(project_dir)
        .remove(session_id)
        .map(|f| f.entry)
        .unwrap_or(HarvestEntry {
            harvested_at: at,
            memories_created: memory_ids.len(),
            memory_ids: memory_ids.to_vec(),
            decision: Some(decision),
            stage: HarvestStage::Collected,
            note,
            archive: None,
        }))
}

/// Attach an archive to a session, creating an unreviewed entry when nobody has
/// reviewed it yet.
///
/// The SessionEnd hook's write, and the one the whole format is shaped around:
/// one `open` and one `write`, no read, no lock. It records the mechanical axis
/// only — an existing decision is not mentioned by the line, so it cannot be
/// reopened however late this lands.
pub fn set_archive(
    project_dir: &Path,
    session_id: &str,
    archive: crate::transcript_archive::ArchiveRef,
) -> Result<()> {
    if !crate::transcripts::is_valid_session_id(session_id) {
        return Ok(());
    }
    let mut line = LedgerLine::touching(session_id);
    line.stage = Some(HarvestStage::Collected);
    line.archive = Some(Some(archive));
    append(project_dir, &[line])
}

/// Move a session to a new stage, leaving its decision alone.
///
/// The other half of the two axes. Writing [`HarvestStage::Compressed`] drains
/// the entry on the next read — see [`drain_and_prune`] for what that costs
/// until the index row exists.
pub fn set_stage(project_dir: &Path, session_id: &str, stage: HarvestStage) -> Result<()> {
    if !crate::transcripts::is_valid_session_id(session_id) {
        return Ok(());
    }
    let mut line = LedgerLine::touching(session_id);
    line.stage = Some(stage);
    append(project_dir, &[line])
}

/// Forget the archive reference for one or more sessions, keeping the review
/// record itself.
///
/// The inverse of [`set_archive`], and mandatory whenever an archive file is
/// deleted: an entry pointing at a file that eviction has already removed
/// makes `harvest ledger show` advertise a transcript that cannot be exported,
/// and turns `export` into a bare "no such file" instead of a clear
/// explanation. Takes a slice because the size-based prune pass evicts many
/// archives at once and should cost one append, not one per file.
///
/// Reads first, and writes only for sessions that actually hold an archive: a
/// blind `archive: null` line would *create* an entry for a session id the log
/// has never heard of, and the prune pass routinely names archives whose entry
/// already aged out.
pub fn clear_archive_refs(project_dir: &Path, session_ids: &[String]) -> Result<()> {
    if session_ids.is_empty() {
        return Ok(());
    }
    let folded = read_folded(project_dir);
    let lines: Vec<LedgerLine> = session_ids
        .iter()
        .filter(|id| folded.get(*id).is_some_and(|f| f.entry.archive.is_some()))
        .map(|id| {
            let mut line = LedgerLine::touching(id);
            line.archive = Some(None);
            line
        })
        .collect();
    if lines.is_empty() {
        return Ok(());
    }
    append(project_dir, &lines)
}

/// Drop archive references whose file is no longer on disk, returning the
/// session ids that lost one.
///
/// [`drain_and_prune`] exempts an entry that names an archive, and
/// `prune_archives` — the only other thing that calls [`clear_archive_refs`] —
/// reports what a *directory scan* found, so it can never report a file that is
/// already gone. A reference the file never caught up with therefore made its
/// entry immortal: exempt from the age window forever, advertising an export
/// that cannot succeed. `projects delete --cascade` removes the archive
/// directory without touching the log, archives written before a
/// `projects link` sit under the child's old id where the root's prune never
/// looks, and eviction on another machine, a restored backup or a manual
/// cleanup do the same.
///
/// Reconciling against the directory rather than stat-ing each file keeps this
/// one `read_dir`, and makes an unreadable-but-present directory an error
/// (nothing is cleared) instead of a mass deletion of references. A file that
/// exists under some *other* project's id is still cleared: every route from a
/// session id to a transcript resolves through the root project id, so such a
/// file is already unreachable, and pretending otherwise is what the entry's
/// immortality was built on.
pub fn reconcile_archive_refs(project_dir: &Path, project_id: &str) -> Result<Vec<String>> {
    let claimed: Vec<String> = read_folded(project_dir)
        .into_iter()
        .filter(|(_, f)| f.entry.archive.is_some())
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

/// What [`clear_harvested`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearOutcome {
    /// No entry for this session; nothing changed.
    NotFound,
    /// The entry held no archive and was tombstoned.
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
/// instead of tombstoning it. An archive is reachable only *through* its ledger
/// entry — `harvest show`, `ledger export` and `ledger rm` all resolve a
/// session id against this map first — so dropping the entry left the `.zst`
/// on disk with no route to it from any command, still counting against the
/// archive budget, while the caller was told the session "will be offered
/// again". For a session whose live transcript Claude Code has already pruned
/// that was false twice over: nothing offered it, and the one surviving copy
/// had just been orphaned. This is the same reasoning [`drain_and_prune`]
/// applies to the age window.
///
/// `Unreviewed` (not removed, not `Deferred`) because it is exactly the state
/// the SessionEnd hook would have left behind: collected, unsettled, and
/// carrying no claim that a human looked at it.
pub fn clear_harvested(project_dir: &Path, session_id: &str) -> Result<ClearOutcome> {
    if session_id.is_empty() {
        return Ok(ClearOutcome::NotFound);
    }
    let folded = read_folded(project_dir);
    let Some(existing) = folded.get(session_id) else {
        return Ok(ClearOutcome::NotFound);
    };
    let mut line = LedgerLine::touching(session_id);
    let outcome = if existing.entry.archive.is_some() {
        line.decision = Some(HarvestDecision::Unreviewed);
        line.memory_ids = Some(Vec::new());
        line.memories_created = Some(0);
        line.note = Some(None);
        ClearOutcome::ResetToUnreviewed
    } else {
        line.removed = true;
        ClearOutcome::Removed
    };
    append(project_dir, &[line])?;
    Ok(outcome)
}

/// Fold a log left at `sub_dir` into the root project's at `root_dir`, then
/// move the old file aside.
///
/// Callers resolve the root themselves, and older versions did not: a project
/// linked with `engramdb projects link` wrote its ledger under its own path
/// while its archives already went to the root. Silently reading the root's
/// ledger from then on would lose every review decision recorded before the
/// fix — sessions re-offered, notes and memory attributions gone — so the old
/// file is adopted rather than abandoned.
///
/// **Adoption is a concatenation, not a merge.** The whole-map format had to
/// reconcile two records of the same session with a four-state precedence
/// function; here each side is already a sequence of timestamped patches, and
/// appending one sequence to the other is the merge — the fold orders by `at`,
/// so it does not matter that the adopted lines land last. That also removes
/// the case the precedence function existed for: a hook's collect line never
/// mentions the decision axis, so it cannot undo a review no matter which side
/// wrote it or when.
///
/// The old file is renamed rather than deleted — this runs unattended from the
/// SessionEnd hook, and nothing here is worth destroying evidence over.
///
/// **Adoption is all-or-nothing, and the move aside is the step that commits
/// it.** Appending first and moving after meant a failed rename was swallowed
/// with the root already written: every later resolve re-appended the same
/// lines, so a `harvest reset` was undone the moment the next harvest command
/// ran and no amount of clearing could settle the session. Renaming first
/// inverts that — a rename that fails adopts nothing, which the caller already
/// treats as advisory ("the old decisions, not the harvest") — and it is also
/// what makes the source safe to read without a lock: once the live path is
/// free, a concurrent writer recreates it with just its own line, which the
/// next adoption picks up.
pub fn adopt_ledger(sub_dir: &Path, root_dir: &Path) -> Result<()> {
    // Compare the resolved *directories*, not the spellings. A textual `==`
    // looks sufficient but is not: the shipped MCP entry is `serve --dir .`, so
    // the invoking dir is literally `"."` while the registry holds the
    // canonical absolute path — the same directory under two names. Under the
    // old locking design that made this function take the same `flock` twice on
    // one inode and hang the SessionEnd hook with the ledger already moved
    // aside; with no lock left it would instead concatenate a log onto itself,
    // doubling every line. Any non-canonical spelling does it: `.`, a relative
    // `--dir`, a symlinked checkout, or `/tmp` -> `/private/tmp` on macOS.
    //
    // `compute_project_id` is deliberately *not* used: it keys off the git
    // remote first, so two different checkouts of one repository share an id
    // and a genuine sub-project would be mistaken for the root.
    let same_dir = match (
        std::fs::canonicalize(sub_dir),
        std::fs::canonicalize(root_dir),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => sub_dir == root_dir,
    };
    if same_dir {
        return Ok(());
    }
    // A sub-project may still be carrying the pre-log JSON map; convert it in
    // place first so adoption has a single thing to move.
    migrate_legacy_map(sub_dir);

    let legacy = ledger_path(sub_dir);
    if !legacy.exists() {
        return Ok(());
    }
    let adopted = legacy.with_extension("jsonl.adopted");
    std::fs::rename(&legacy, &adopted)?;

    let raw = std::fs::read_to_string(&adopted).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(());
    }
    // Verbatim, so tombstones and clears travel too. The newline matters: if
    // the adopted file's last line was torn, concatenating without one would
    // splice it onto the next record appended to the root and take that record
    // down with it. Terminated, the torn fragment is one skipped line.
    let mut body = raw;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    crate::state_file::append_state_file(&ledger_path(root_dir), &body)?;
    maybe_compact(root_dir);
    Ok(())
}

/// Read the pre-log JSON map once, append it as lines, and retire the file.
///
/// Existing users must not lose their review decisions to a format change. The
/// rename comes **first** and is what commits the migration: it is atomic, so
/// exactly one process converts the map even if several start at once, and a
/// crash afterwards leaves the data under `.migrated` rather than nowhere.
///
/// Best-effort by design — this runs from the unattended SessionEnd hook, and a
/// migration that cannot proceed must not fail a harvest. Every way of losing
/// an entry here is reported.
fn migrate_legacy_map(project_dir: &Path) {
    let legacy = legacy_path(project_dir);
    if !legacy.exists() {
        return;
    }
    let retired = legacy.with_extension("json.migrated");
    if let Err(e) = std::fs::rename(&legacy, &retired) {
        tracing::warn!(
            "harvest ledger: could not retire the old map at {} ({e}); \
             its review decisions are not in the log yet",
            legacy.display()
        );
        return;
    }
    let raw = match std::fs::read_to_string(&retired) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!(
                "harvest ledger: the old map at {} could not be read ({e}); \
                 its review decisions are lost, but the file is still there",
                retired.display()
            );
            return;
        }
    };
    // Per-entry, not whole-map: one unreadable record from a future version
    // would otherwise discard every decision in the file, which is the exact
    // data loss the quarantine path existed to soften.
    let map: HashMap<String, serde_json::Value> = match serde_json::from_str(&raw) {
        Ok(map) => map,
        Err(e) => {
            tracing::warn!(
                "harvest ledger: the old map at {} is not a JSON object ({e}); \
                 nothing was migrated. The file is kept as-is.",
                retired.display()
            );
            return;
        }
    };
    let mut lines: Vec<LedgerLine> = Vec::new();
    for (session_id, value) in map {
        if !crate::transcripts::is_valid_session_id(&session_id) {
            tracing::warn!(
                "harvest ledger: dropping migrated entry {session_id:?} — not a session id"
            );
            continue;
        }
        let entry: LegacyEntry = match serde_json::from_value(value) {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!(
                    "harvest ledger: dropping migrated entry {session_id} ({e}); \
                     the original is kept at {}",
                    retired.display()
                );
                continue;
            }
        };
        // The old format inferred a missing decision from the memory count on
        // every read. Resolving it once, here, is what lets `decision: None`
        // mean exactly `Unreviewed` from now on.
        let decision = entry.decision.unwrap_or(if entry.memories_created > 0 {
            HarvestDecision::Harvested
        } else {
            HarvestDecision::Skipped
        });
        lines.push(LedgerLine {
            session_id,
            // The recorded time, not now: the age window is measured against
            // it, and re-stamping would make every ancient entry look fresh.
            at: entry.harvested_at,
            stage: Some(HarvestStage::Collected),
            decision: Some(decision),
            memory_ids: Some(entry.memory_ids),
            memories_created: Some(entry.memories_created),
            note: Some(entry.note),
            archive: Some(entry.archive),
            removed: false,
        });
    }
    if lines.is_empty() {
        return;
    }
    if let Err(e) = append(project_dir, &lines) {
        tracing::warn!(
            "harvest ledger: could not write {} migrated entr{} ({e}); \
             the original map is kept at {}",
            lines.len(),
            if lines.len() == 1 { "y" } else { "ies" },
            retired.display()
        );
    }
}

/// One record of the whole-map JSON format, as it was last written.
#[derive(Deserialize)]
struct LegacyEntry {
    #[serde(default = "Utc::now")]
    harvested_at: DateTime<Utc>,
    #[serde(default)]
    memories_created: usize,
    #[serde(default)]
    memory_ids: Vec<String>,
    #[serde(default)]
    decision: Option<HarvestDecision>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    archive: Option<crate::transcript_archive::ArchiveRef>,
}

/// Append `lines` as one write, then consider compacting.
fn append(project_dir: &Path, lines: &[LedgerLine]) -> Result<()> {
    let mut body = String::new();
    for line in lines {
        let encoded = serde_json::to_string(line)
            .map_err(|e| crate::error::StorageError::Validation(e.to_string()))?;
        body.push_str(&encoded);
        body.push('\n');
    }
    crate::state_file::append_state_file(&ledger_path(project_dir), &body)?;
    maybe_compact(project_dir);
    Ok(())
}

/// Rewrite the log as one line per live entry once it has outgrown them.
///
/// Purely a space reclaim — the fold is identical either way — so it is
/// best-effort throughout and gives up rather than risk a record. Two hazards,
/// both handled by reading the tail rather than by taking a lock:
///
/// 1. Appends landing *while* the snapshot is being built are carried over
///    verbatim, re-read until the file stops growing.
/// 2. An append landing between the last check and the `rename` would go to the
///    inode the rename unlinks. The open handle taken beforehand still sees
///    those bytes, so they are recovered onto the new file afterwards.
///
/// What is left is the writer that opened before the rename and had not
/// finished when the recovery read ran: microseconds, once per compaction. It
/// is a real (if small) loss path and this is where it is declared.
fn maybe_compact(project_dir: &Path) {
    let path = ledger_path(project_dir);
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    if meta.len() < COMPACT_MIN_BYTES {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let lines = raw.lines().filter(|l| !l.trim().is_empty()).count();
    let mut folded = fold(parse_lines(&raw));
    drain_and_prune(&mut folded);
    if lines <= folded.len().max(1) * COMPACT_FACTOR {
        return;
    }

    // Sorted, so the rewritten file is reproducible and diffable.
    let mut ids: Vec<&String> = folded.keys().collect();
    ids.sort();
    let mut body = String::new();
    for id in ids {
        let f = &folded[id];
        let entry = &f.entry;
        let line = LedgerLine {
            session_id: id.clone(),
            // The high-water mark, not `harvested_at`: the snapshot subsumes
            // every line folded into it, so it has to sort after all of them or
            // a concurrent append could beat data that is newer than itself.
            at: f.last_at,
            stage: Some(entry.stage),
            decision: Some(entry.decision()),
            memory_ids: Some(entry.memory_ids.clone()),
            memories_created: Some(entry.memories_created),
            note: Some(entry.note.clone()),
            archive: Some(entry.archive.clone()),
            removed: false,
        };
        let Ok(encoded) = serde_json::to_string(&line) else {
            return;
        };
        body.push_str(&encoded);
        body.push('\n');
    }

    // Carry over anything appended while the snapshot was being built, and keep
    // going until the file stops moving. Still growing after a few passes means
    // a busy writer, and leaving the file long is the harmless outcome.
    let Ok(mut handle) = std::fs::File::open(&path) else {
        return;
    };
    let mut consumed = raw.len() as u64;
    let mut settled = false;
    for _ in 0..COMPACT_TAIL_ATTEMPTS {
        let Ok(meta) = handle.metadata() else {
            return;
        };
        if meta.len() == consumed {
            settled = true;
            break;
        }
        if meta.len() < consumed {
            // Somebody else rewrote it underneath us; theirs is as good as ours.
            return;
        }
        let Ok(tail) = read_from(&mut handle, consumed) else {
            return;
        };
        consumed += tail.len() as u64;
        body.push_str(&tail);
    }
    if !settled {
        return;
    }
    if crate::state_file::write_state_file(&path, &body).is_err() {
        return;
    }
    // The unlinked inode is still open here, so a write that raced the rename
    // is visible and can be moved onto the new file.
    if let Ok(tail) = read_from(&mut handle, consumed) {
        if !tail.is_empty() {
            let _ = crate::state_file::append_state_file(&path, &tail);
        }
    }
}

/// Read `handle` from `offset` to EOF as text, leaving the cursor at the end.
fn read_from(handle: &mut std::fs::File, offset: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    handle.seek(SeekFrom::Start(offset))?;
    let mut out = String::new();
    handle.read_to_string(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn lines_in(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(ledger_path(dir))
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
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
    fn a_write_appends_and_never_rewrites() {
        // The property the whole format exists for: the SessionEnd hook's write
        // must not depend on, or disturb, what is already in the file. If any
        // writer rewrites, two of them racing lose an entry — which is what the
        // deleted `flock` was there to prevent.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        mark_harvested(dir, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();
        let before = std::fs::read_to_string(ledger_path(dir)).unwrap();
        // Comparing content afterwards is not enough on its own: a
        // read-modify-write that rewrites the same bytes plus one line looks
        // identical from the outside, and is exactly the shape that drops a
        // concurrent writer's record. Holding a handle open across the write
        // reproduces the loss directly — every whole-file writer here goes
        // temp-then-rename, so a rewrite leaves this handle on an unlinked
        // inode that will never see the new line, just as a racing appender's
        // handle would not.
        let mut open_before = std::fs::File::open(ledger_path(dir)).unwrap();

        set_archive(dir, "s2", sample_archive()).unwrap();
        set_stage(dir, "s1", HarvestStage::Indexed).unwrap();

        let after = std::fs::read_to_string(ledger_path(dir)).unwrap();
        assert!(
            after.starts_with(&before),
            "an existing byte of the log changed; writes must be pure appends"
        );
        assert_eq!(
            lines_in(dir).len(),
            3,
            "each transition must cost exactly one line"
        );
        assert_eq!(
            read_from(&mut open_before, 0).unwrap(),
            after,
            "the writes went to a different file than the one already open; \
             a concurrent writer's record would go down with it"
        );
    }

    #[test]
    fn later_lines_win_field_by_field() {
        // "Last-write-wins per session id" is per *field*: the hook's collect
        // line and the human's decision line are two axes on one session, and
        // whichever lands second must not erase the other. The old format
        // needed a lock and a merge function for this.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        mark_harvested(dir, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();
        set_archive(dir, "s1", sample_archive()).unwrap();
        mark_harvested(
            dir,
            "s1",
            &[],
            HarvestDecision::Deferred,
            Some("later".into()),
        )
        .unwrap();

        let entry = &read_harvested(dir)["s1"];
        assert_eq!(entry.decision(), HarvestDecision::Deferred);
        assert!(entry.memory_ids.is_empty());
        assert_eq!(entry.note.as_deref(), Some("later"));
        assert!(
            entry.archive.is_some(),
            "a decision line erased an archive it never mentioned"
        );
    }

    #[test]
    fn stage_and_decision_are_independent_axes() {
        // Normative in the spec: a session can be `indexed` while `skipped`,
        // or `compressed` while `deferred`. Collapsing them into one word is
        // what makes "reviewed and found nothing" indistinguishable from
        // "never looked at".
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        set_archive(dir, "s1", sample_archive()).unwrap();
        assert_eq!(read_harvested(dir)["s1"].stage, HarvestStage::Collected);
        assert_eq!(
            read_harvested(dir)["s1"].decision(),
            HarvestDecision::Unreviewed
        );

        mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None).unwrap();
        assert_eq!(
            read_harvested(dir)["s1"].stage,
            HarvestStage::Collected,
            "a decision moved the mechanical axis"
        );

        set_stage(dir, "s1", HarvestStage::Indexed).unwrap();
        let entry = &read_harvested(dir)["s1"];
        assert_eq!(entry.stage, HarvestStage::Indexed);
        assert_eq!(
            entry.decision(),
            HarvestDecision::Skipped,
            "a stage move rewrote the human's decision"
        );
    }

    #[test]
    fn an_entry_that_reaches_compressed_leaves_the_ledger() {
        // The drain. For step 1 the target is simply "gone" — the index row
        // that is supposed to carry it arrives later — so this is a real loss
        // and `drain_and_prune` says so rather than dropping it quietly.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        mark_harvested(dir, "s1", &["m1".into()], HarvestDecision::Harvested, None).unwrap();
        set_archive(dir, "s1", sample_archive()).unwrap();
        assert!(read_harvested(dir).contains_key("s1"));

        set_stage(dir, "s1", HarvestStage::Compressed).unwrap();
        assert!(
            !read_harvested(dir).contains_key("s1"),
            "a compressed entry stayed in the ledger"
        );
        assert!(
            !is_harvested(dir, "s1"),
            "a drained session must not still read as settled"
        );
        // Drained on read, not on write: the bytes are still there for the
        // index step to pick up, and compaction is what reclaims them.
        assert!(
            lines_in(dir).iter().any(|l| l.contains("compressed")),
            "the transition itself must still be on disk"
        );
    }

    /// Append to `session_id` until the file gets shorter, i.e. until a
    /// compaction lands, and report the line count just before it did.
    ///
    /// Watching for the rewrite beats asserting a line count after a fixed
    /// number of writes: the file is bounded by `max(COMPACT_MIN_BYTES,
    /// factor × entries)`, so where it happens to sit at the end of a loop is
    /// an artifact of how the last write divided into the byte gate.
    fn append_until_compacted(dir: &Path, session_id: &str, note: &str) -> usize {
        for _ in 0..512 {
            let before = lines_in(dir).len();
            mark_harvested(
                dir,
                session_id,
                &[],
                HarvestDecision::Deferred,
                Some(note.to_string()),
            )
            .unwrap();
            if lines_in(dir).len() < before {
                return before;
            }
        }
        panic!("the log grew without ever being compacted");
    }

    #[test]
    fn the_log_is_compacted_once_it_outgrows_its_live_entries() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Long notes so the byte gate is reached in a sane number of writes.
        let filler = "x".repeat(2048);
        let before = append_until_compacted(dir, "s1", &filler);

        assert!(
            before > COMPACT_FACTOR,
            "the log was rewritten before it had outgrown its one entry ({before} lines)"
        );
        assert_eq!(
            lines_in(dir).len(),
            1,
            "compaction must leave exactly one line per live entry"
        );
        // ...and the fold survived it.
        let entry = &read_harvested(dir)["s1"];
        assert_eq!(entry.decision(), HarvestDecision::Deferred);
        assert_eq!(entry.note.as_deref(), Some(filler.as_str()));
    }

    #[test]
    fn compaction_drops_dead_entries_but_keeps_live_ones() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let filler = "x".repeat(2048);
        for n in 0..8 {
            let id = format!("dead{n}");
            mark_harvested(
                dir,
                &id,
                &[],
                HarvestDecision::Skipped,
                Some(filler.clone()),
            )
            .unwrap();
            clear_harvested(dir, &id).unwrap();
        }
        append_until_compacted(dir, "alive", &filler);

        let map = read_harvested(dir);
        assert_eq!(map.len(), 1, "tombstoned entries came back");
        assert!(map.contains_key("alive"));
        assert!(
            !lines_in(dir).iter().any(|l| l.contains("dead0")),
            "compaction kept a session it had already applied a tombstone for"
        );
    }

    #[test]
    fn a_torn_final_line_does_not_destroy_the_log() {
        // A crash mid-append leaves a partial record. Under the whole-map
        // format that was the entire file's worth of review decisions; here it
        // must cost exactly the torn line.
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
        mark_harvested(dir, "also-me", &[], HarvestDecision::Skipped, None).unwrap();

        let path = ledger_path(dir);
        let raw = std::fs::read_to_string(&path).unwrap();
        let torn = format!("{raw}{{\"session_id\":\"tor");
        std::fs::write(&path, torn).unwrap();

        let map = read_harvested(dir);
        assert!(
            map.contains_key("keep-me"),
            "a torn line took the log with it"
        );
        assert!(map.contains_key("also-me"));
        assert_eq!(map.len(), 2);
        assert_eq!(map["keep-me"].memory_ids, vec!["m1"]);

        // And a later write still lands cleanly on top of the wreckage.
        mark_harvested(dir, "after", &[], HarvestDecision::Skipped, None).unwrap();
        assert!(is_harvested(dir, "after"));
        assert!(is_harvested(dir, "keep-me"));
    }

    #[test]
    fn a_forward_incompatible_line_costs_only_itself() {
        // Deserialization is all-or-nothing *per line* now. Under the map it
        // was all-or-nothing per file, which is why that path needed a
        // quarantine copy to be survivable at all.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None).unwrap();

        let path = ledger_path(dir);
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str(
            "{\"session_id\":\"s2\",\"at\":\"2026-07-01T00:00:00Z\",\
             \"decision\":\"from_the_future_v9\"}\n",
        );
        std::fs::write(&path, raw).unwrap();

        let map = read_harvested(dir);
        assert!(map.contains_key("s1"), "one bad line discarded the log");
        assert!(!map.contains_key("s2"));
    }

    #[test]
    fn concurrent_appends_do_not_lose_an_entry() {
        // Two SessionEnd hooks ending at once is the ordinary case, and the
        // read-modify-write this format replaced dropped one of them whenever
        // the advisory lock could not be taken. Nothing is locked here: the
        // guarantee is that each record is one `O_APPEND` write.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(ledger_path(&dir).parent().unwrap()).unwrap();

        let threads: Vec<_> = (0..8)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for n in 0..16 {
                        let id = format!("s{t}-{n}");
                        if n % 2 == 0 {
                            set_archive(&dir, &id, sample_archive()).unwrap();
                        } else {
                            mark_harvested(&dir, &id, &[], HarvestDecision::Skipped, None).unwrap();
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let map = read_harvested(&dir);
        assert_eq!(map.len(), 8 * 16, "a concurrent append was lost");
        for t in 0..8 {
            for n in 0..16 {
                let id = format!("s{t}-{n}");
                let entry = map.get(&id).unwrap_or_else(|| panic!("{id} is missing"));
                if n % 2 == 0 {
                    assert!(entry.archive.is_some(), "{id} lost its archive");
                } else {
                    assert_eq!(entry.decision(), HarvestDecision::Skipped, "{id}");
                }
            }
        }
    }

    #[test]
    fn a_legacy_json_map_is_migrated_into_the_log() {
        // Existing users must not lose review decisions to the format change.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let legacy = legacy_path(dir);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            r#"{
              "old_harvested": {"harvested_at":"2026-07-01T00:00:00Z","memories_created":2},
              "old_empty":     {"harvested_at":"2026-07-01T00:00:00Z","memories_created":0},
              "old_deferred":  {"harvested_at":"2026-07-01T00:00:00Z","decision":"deferred",
                                "note":"come back to this"}
            }"#,
        )
        .unwrap();

        let map = read_harvested(dir);
        // The decision the old format inferred on every read is resolved once,
        // here, so `None` can mean exactly `Unreviewed` from now on.
        assert_eq!(map["old_harvested"].decision(), HarvestDecision::Harvested);
        assert_eq!(map["old_harvested"].memories_created, 2);
        assert_eq!(map["old_empty"].decision(), HarvestDecision::Skipped);
        assert_eq!(map["old_deferred"].decision(), HarvestDecision::Deferred);
        assert_eq!(
            map["old_deferred"].note.as_deref(),
            Some("come back to this")
        );

        assert!(!legacy.exists(), "the old map is still being read");
        assert!(
            legacy.with_extension("json.migrated").exists(),
            "the old map was destroyed rather than kept"
        );
        // Idempotent, and a later write does not resurrect it.
        mark_harvested(dir, "new", &[], HarvestDecision::Skipped, None).unwrap();
        assert_eq!(read_harvested(dir).len(), 4);
    }

    #[test]
    fn migration_keeps_the_entries_around_an_unreadable_one() {
        // Per-entry, not whole-map: one record from a future version must not
        // cost every decision in the file.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let legacy = legacy_path(dir);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            r#"{
              "good": {"harvested_at":"2026-07-01T00:00:00Z","memories_created":1},
              "bad":  {"harvested_at":"2026-07-01T00:00:00Z","decision":"from_the_future_v9"}
            }"#,
        )
        .unwrap();

        let map = read_harvested(dir);
        assert!(map.contains_key("good"), "one bad record discarded the map");
        assert!(!map.contains_key("bad"));
        // The evidence survives, so the loss is recoverable by hand.
        assert!(
            std::fs::read_to_string(legacy.with_extension("json.migrated"))
                .unwrap()
                .contains("from_the_future_v9")
        );
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
        // out; that must not be an error, and — the part a blind append would
        // get wrong — must not create an entry.
        clear_archive_refs(dir, &["ghost".to_string()]).unwrap();
        clear_archive_refs(dir, &[]).unwrap();
        assert!(read_harvested(dir).is_empty());
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

        let mut archived = LedgerLine::touching("archived");
        archived.at = long_ago;
        archived.decision = Some(HarvestDecision::Harvested);
        archived.memory_ids = Some(vec!["m1".into()]);
        archived.archive = Some(Some(sample_archive()));
        let mut bare = LedgerLine::touching("no-archive");
        bare.at = long_ago;
        bare.decision = Some(HarvestDecision::Skipped);
        append(dir, &[archived, bare]).unwrap();

        set_archive(dir, "fresh", sample_archive()).unwrap();

        let after = read_harvested(dir);
        let entry = after
            .get("archived")
            .expect("an entry pointing at a live archive was aged out, orphaning the file");
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
    fn adopting_a_sub_project_ledger_concatenates_it_and_moves_the_file_aside() {
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
            ledger_path(&sub).with_extension("jsonl.adopted").exists(),
            "the old file must be kept, not deleted"
        );

        // Idempotent: nothing left to adopt, and the root is untouched.
        adopt_ledger(&sub, &root).unwrap();
        assert_eq!(read_harvested(&root).len(), 2);
    }

    #[test]
    fn adoption_does_not_let_a_hook_archive_undo_a_review() {
        // The SessionEnd hook writes to the root for every session it archives,
        // so the root's line is routinely *newer* than the review recorded on
        // the sub-project side. The whole-map format needed a four-state
        // precedence function for this; here the hook's line simply does not
        // mention the decision axis, so there is nothing to reconcile.
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
            "the archive reference must survive adoption"
        );
    }

    #[test]
    fn adoption_keeps_a_later_deferral_over_an_older_settled_decision() {
        // The user reviewed the session on the sub-project, then asked for
        // another look from the root. The adopted lines land *last* in the
        // file, so this only holds because the fold orders by the record's own
        // timestamp rather than by its position.
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

    #[test]
    #[cfg(unix)]
    fn adopting_into_the_same_directory_under_another_name_is_a_no_op() {
        // The shipped MCP entry is `serve --dir .`, so the sub and the root are
        // routinely the same directory spelled two ways. Under the old locking
        // design a textual `==` here deadlocked the hook; with no lock left it
        // instead retires the live log and rebuilds it from the copy on every
        // command — a window in which the ledger does not exist, and a growing
        // pile of `.adopted` files. Any non-canonical spelling does it: `.`, a
        // relative `--dir`, a symlinked checkout, `/tmp` -> `/private/tmp`.
        let tmp = TempDir::new().unwrap();
        let dir = &tmp.path().join("checkout");
        std::fs::create_dir_all(dir).unwrap();
        mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None).unwrap();
        let before = lines_in(dir);

        // A symlinked spelling, not `foo/.`: Rust's `Path` equality compares
        // *components* and already folds a bare `.` away, so a dotted path
        // would pass a textual `==` too and prove nothing.
        let aliased = tmp.path().join("alias");
        std::os::unix::fs::symlink(dir, &aliased).unwrap();
        adopt_ledger(&aliased, dir).unwrap();

        assert_eq!(
            lines_in(dir),
            before,
            "the log was concatenated onto itself"
        );
        assert!(ledger_path(dir).exists(), "the live log was moved aside");
        assert!(
            !ledger_path(dir).with_extension("jsonl.adopted").exists(),
            "the log adopted itself: the two spellings were compared textually"
        );
    }

    #[test]
    fn adoption_carries_over_a_sub_projects_legacy_map() {
        // A project linked before the format change has a JSON map, not a log.
        // Leaving it behind loses exactly the decisions adoption exists for.
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("child");
        let root = tmp.path().join("root");
        let legacy = legacy_path(&sub);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &legacy,
            r#"{"old": {"harvested_at":"2026-07-01T00:00:00Z","memories_created":1}}"#,
        )
        .unwrap();

        adopt_ledger(&sub, &root).unwrap();

        let merged = read_harvested(&root);
        assert!(
            merged.contains_key("old"),
            "a sub-project's pre-log map was left behind"
        );
        assert_eq!(merged["old"].decision(), HarvestDecision::Harvested);
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
        std::fs::create_dir_all(ledger_path(&sub).with_extension("jsonl.adopted")).unwrap();

        assert!(
            adopt_ledger(&sub, &root).is_err(),
            "a failed move aside must be reported, not swallowed"
        );
        assert!(
            !read_harvested(&root).contains_key("s1"),
            "the lines were appended even though the sub-ledger could not be retired"
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
    fn a_dangling_archive_reference_stops_exempting_its_entry() {
        // The age exemption is written against the entry's *claim*, and nothing
        // used to reconcile the claim with the disk: `prune_archives` reports
        // only files a directory scan found, so a reference whose file went
        // another way — `projects delete --cascade`, an archive written under
        // the pre-`projects link` id, eviction on another machine — never
        // reached `clear_archive_refs` and made its entry immortal.
        let tmp = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let prev = std::env::var("ENGRAMDB_DATA_DIR").ok();
        std::env::set_var("ENGRAMDB_DATA_DIR", data.path());

        let dir = tmp.path();
        let mut ghost = LedgerLine::touching("ghost");
        ghost.at = Utc::now() - Duration::days(PRUNE_AFTER_DAYS + 35);
        ghost.decision = Some(HarvestDecision::Skipped);
        ghost.archive = Some(Some(sample_archive()));
        append(dir, &[ghost]).unwrap();

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
    fn empty_session_id_is_rejected() {
        let tmp = TempDir::new().unwrap();
        assert!(mark_harvested(tmp.path(), "", &[], HarvestDecision::Skipped, None).is_err());
        assert!(!is_harvested(tmp.path(), ""));
        assert!(!clear_harvested(tmp.path(), "").unwrap().changed());
    }

    /// A symlink committed at the ledger's path arrives in every clone
    /// (`.engramdb/` is tracked and `init` never overwrites an existing
    /// `.gitignore`), and the SessionEnd hook writes here unattended — so
    /// following one is an arbitrary-file *append* that needs no local access
    /// at all. The whole-map format was covered by its temp-then-rename; an
    /// append opens the live path directly, so it needs `O_NOFOLLOW` of its
    /// own.
    #[test]
    #[cfg(unix)]
    fn a_planted_symlink_cannot_redirect_an_append() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();

        let path = ledger_path(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();

        let result = mark_harvested(dir, "s1", &[], HarvestDecision::Skipped, None);
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the ledger write landed on the symlink's target"
        );
        assert!(result.is_err(), "a redirected write reported success");
    }
}
