//! Per-memory usage events written by short-lived processes, and the
//! combined read over the table and the not-yet-drained log.
//!
//! The CLI, the MCP server and hooks running in their own process record a
//! [`MemoryEvent`] by appending one JSON line to the owning store's
//! `.engramdb/state/memory_events.jsonl` through [`crate::state_file`]: one
//! `open` + `write` for all of a call's events, no lock, inside a hook's
//! 15–20 ms budget. A long-running process later moves the lines into the
//! `memory_events` table ([`super::memory_events`]) with [`drain_to_table`].
//! Hooks the daemon serves skip the file and buffer in the daemon instead.
//!
//! **Declared loss paths.** All of these lose events, by design:
//! - **Rotation, the rotation race and the drain grace**, as documented once
//!   for every JSONL log in [`super::jsonl_log`]. Without a drainer, history
//!   is at most two generations of [`ROTATE_BYTES`] each (~8 MiB, roughly
//!   40,000 events).
//! - **A killed daemon** loses at most one flush interval of buffered
//!   `injected` events (`SIGKILL`; every orderly shutdown flushes).
//! - **A lost daemon reply.** When a hook's reply is lost, the client neither
//!   retries nor records: the daemon may or may not have recorded it.
//! - **Stats disabled.** `[stats].enabled = false` records nothing at all.
//! - **Uninitialized stores** record nothing: the plugin registers hooks
//!   machine-wide, and every directory Claude Code starts in would otherwise
//!   collect a state folder (the same test SessionEnd uses).
//! - **A torn write** costs the torn line, which [`load_memory_events`] skips
//!   and counts.
//!
//! **No double counting.** Reads combine the table with the unclaimed JSONL
//! files. A drain claims (renames) a file before it inserts, and reads never
//! look at a claimed file, so an event is in exactly one of the two places a
//! read looks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::Result;
use crate::telemetry::jsonl_log::{is_initialized, state_dir, JsonlLog};
use crate::telemetry::memory_events::{
    ingest_memory_events, load_series_table, load_table_usage, returned_sessions_table,
    DailySeries, MemoryUsage, SERIES_DAYS,
};

pub use crate::telemetry::memory_events::{
    load_rollup, optimize_memory_events, prune_memory_events, DailyUsage, MemoryEvent, Surface,
    UsageCounts, UsageKind,
};

/// Live log, under the owning store's `.engramdb/state/`.
pub const MEMORY_EVENTS_FILE: &str = "memory_events.jsonl";

/// Previous generation, kept after rotation.
pub const MEMORY_EVENTS_ROTATED_FILE: &str = "memory_events.1.jsonl";

/// Prefix of a file a drain has claimed and not yet deleted.
pub const MEMORY_EVENTS_DRAINING_PREFIX: &str = "memory_events.draining.";

/// Size at which the live file rotates. Larger than the hook log's: one
/// query writes a line per returned memory.
pub const ROTATE_BYTES: u64 = 4 * 1024 * 1024;

/// How long a drain waits between claiming a file and reading it.
pub const DRAIN_GRACE: Duration = Duration::from_millis(100);

const LOG: JsonlLog = JsonlLog {
    live: MEMORY_EVENTS_FILE,
    rotated: MEMORY_EVENTS_ROTATED_FILE,
    draining_prefix: MEMORY_EVENTS_DRAINING_PREFIX,
    lock_file: "memory_events_drain.lock",
};

/// Path of the live usage log for a store root.
pub fn memory_events_path(store_root: &Path) -> PathBuf {
    LOG.live_path(store_root)
}

/// Append `events` to the log of the store rooted at `store_root`, in one
/// write. Does nothing for an empty slice or an uninitialized store. The
/// caller has already checked `[stats].enabled`. Callers treat an error as
/// non-fatal.
pub fn record_memory_events(store_root: &Path, events: &[MemoryEvent]) -> Result<()> {
    record_at(store_root, events, ROTATE_BYTES)
}

fn record_at(store_root: &Path, events: &[MemoryEvent], rotate_bytes: u64) -> Result<()> {
    if events.is_empty() || !is_initialized(store_root) {
        return Ok(());
    }
    let mut lines = String::new();
    for e in events {
        lines.push_str(&serde_json::to_string(e)?);
        lines.push('\n');
    }
    LOG.append(store_root, &lines, rotate_bytes)
}

/// What [`load_memory_events`] read back.
#[derive(Debug, Default)]
pub struct LoadedMemoryEvents {
    pub events: Vec<MemoryEvent>,
    /// Lines that did not parse (torn writes, another version's format).
    pub skipped_lines: u64,
}

fn parse_lines(text: &str, out: &mut LoadedMemoryEvents) {
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<MemoryEvent>(line) {
            Ok(e) => out.events.push(e),
            Err(_) => out.skipped_lines += 1,
        }
    }
}

/// Read both unclaimed generations, oldest first.
pub fn load_memory_events(store_root: &Path) -> Result<LoadedMemoryEvents> {
    let mut out = LoadedMemoryEvents::default();
    for text in LOG.read_unclaimed(store_root)? {
        parse_lines(&text, &mut out);
    }
    Ok(out)
}

fn load_or_warn(store_root: &Path) -> LoadedMemoryEvents {
    match load_memory_events(store_root) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("usage stats: could not read the usage log: {e}");
            LoadedMemoryEvents::default()
        }
    }
}

/// What one [`drain_to_table`] call moved.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Events handed to the table (already-known ids are ignored there).
    pub events: u64,
    /// Lines that did not parse and were dropped with their file.
    pub skipped_lines: u64,
    /// Files claimed and deleted.
    pub files: u64,
}

/// Move the store's JSONL usage log into its `memory_events` table and
/// refresh the rollup rows it touched. The claim / grace / insert / delete
/// sequence is [`JsonlLog::drain`]'s; the insert is idempotent on `id`.
pub async fn drain_to_table(store_root: &Path, store_id: &str) -> anyhow::Result<DrainReport> {
    drain_with_grace(store_root, store_id, DRAIN_GRACE).await
}

async fn drain_with_grace(
    store_root: &Path,
    store_id: &str,
    grace: Duration,
) -> anyhow::Result<DrainReport> {
    let drained = LOG
        .drain(store_root, store_id, grace, |texts| async move {
            let mut loaded = LoadedMemoryEvents::default();
            for text in &texts {
                parse_lines(text, &mut loaded);
            }
            // A line naming another store is a writer's bug; it still belongs
            // to this file's store, which is where its count must land.
            for e in &mut loaded.events {
                if e.store_id != store_id {
                    e.store_id = store_id.to_string();
                }
            }
            ingest_memory_events(store_id, &loaded.events, true).await?;
            Ok(DrainReport {
                events: loaded.events.len() as u64,
                skipped_lines: loaded.skipped_lines,
                files: 0,
            })
        })
        .await?;
    let Some((mut report, files)) = drained else {
        return Ok(DrainReport::default());
    };
    report.files = files;
    if report.skipped_lines > 0 {
        tracing::warn!(
            "usage stats: dropped {} unreadable line(s) while draining {}",
            report.skipped_lines,
            state_dir(store_root).display()
        );
    }
    Ok(report)
}

/// Usage of a store's memories: the table plus the unclaimed JSONL lines.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageView {
    pub store_id: String,
    pub by_memory: BTreeMap<String, MemoryUsage>,
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped_lines: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Exact usage for `memory` (a full id), or for every memory of the store,
/// combining the tables with the not-yet-drained log. Nothing is counted
/// twice (see the module docs), and distinct sessions are counted across both
/// sources. A failure on either side is logged and that side reads empty.
pub async fn load_usage(store_root: &Path, store_id: &str, memory: Option<&str>) -> UsageView {
    let mut table = match load_table_usage(store_id, memory).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("usage stats: could not read memory_events: {e}");
            Default::default()
        }
    };
    let loaded = load_or_warn(store_root);
    for e in &loaded.events {
        if memory.is_none_or(|m| m == e.memory_id) {
            table.add(e);
        }
    }
    UsageView {
        store_id: store_id.to_string(),
        by_memory: table.finish(store_id),
        skipped_lines: loaded.skipped_lines,
    }
}

/// A dense daily series of the last `days` days (UTC, today included), for
/// `memory` or the whole store, from the table plus the undrained log.
pub async fn load_daily_series(
    store_root: &Path,
    store_id: &str,
    memory: Option<&str>,
    days: u32,
) -> Vec<DailyUsage> {
    load_daily_series_at(store_root, store_id, memory, days, Utc::now()).await
}

async fn load_daily_series_at(
    store_root: &Path,
    store_id: &str,
    memory: Option<&str>,
    days: u32,
    now: DateTime<Utc>,
) -> Vec<DailyUsage> {
    let mut series = DailySeries::empty(days, now.date_naive());
    if let Err(e) = load_series_table(store_id, memory, &mut series).await {
        tracing::warn!("usage stats: could not read the daily series: {e}");
    }
    for e in &load_or_warn(store_root).events {
        if memory.is_none_or(|m| m == e.memory_id) {
            series.add(e.kind, e.ts);
        }
    }
    series.days
}

/// The default series length.
pub const DEFAULT_SERIES_DAYS: u32 = SERIES_DAYS;

/// The distinct sessions per memory whose `returned` events are at or after
/// `since`, from the table and the undrained log: the §11.3 promotion input.
pub async fn returned_sessions(
    store_root: &Path,
    store_id: &str,
    since: Option<DateTime<Utc>>,
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let mut out = match returned_sessions_table(store_id, since).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("usage stats: could not read returned sessions: {e}");
            Default::default()
        }
    };
    for e in load_or_warn(store_root).events {
        if e.kind != UsageKind::Returned || since.is_some_and(|s| e.ts < s) {
            continue;
        }
        if let Some(s) = e.session_id {
            out.entry(e.memory_id).or_default().insert(s);
        }
    }
    out
}

/// Which read signal orders a top-N listing. The other two break ties, in
/// the order fetched, injected, returned; the signals are never added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSort {
    #[default]
    Fetched,
    Injected,
    Returned,
}

impl UsageSort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fetched" => Some(Self::Fetched),
            "injected" => Some(Self::Injected),
            "returned" => Some(Self::Returned),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fetched => "fetched",
            Self::Injected => "injected",
            Self::Returned => "returned",
        }
    }

    fn key(&self, c: &UsageCounts) -> (u64, u64, u64) {
        match self {
            Self::Fetched => (c.fetched, c.injected, c.returned),
            Self::Injected => (c.injected, c.fetched, c.returned),
            Self::Returned => (c.returned, c.fetched, c.injected),
        }
    }
}

/// The `n` most-used memories by `sort`, most used first, ties broken by id.
/// Memories with no read event at all are left out.
pub fn top_memories(view: &UsageView, n: usize, sort: UsageSort) -> Vec<MemoryUsage> {
    let mut rows: Vec<&MemoryUsage> = view
        .by_memory
        .values()
        .filter(|u| u.counts.returned + u.counts.injected + u.counts.fetched > 0)
        .collect();
    rows.sort_by(|a, b| {
        sort.key(&b.counts)
            .cmp(&sort.key(&a.counts))
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });
    rows.into_iter().take(n).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".engramdb/memories")).unwrap();
        tmp
    }

    /// A store whose global data dir exists, as `MemoryStore::init` leaves it.
    fn store_with_id() -> (TempDir, String) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let sid = format!("test-usage-{:016x}", N.fetch_add(1, Ordering::Relaxed));
        let data = crate::paths::global_data_dir()
            .unwrap()
            .join("projects")
            .join(&sid);
        std::fs::create_dir_all(&data).unwrap();
        (project(), sid)
    }

    fn ev(sid: &str, mid: &str, kind: UsageKind, session: Option<&str>) -> MemoryEvent {
        MemoryEvent::new(mid, sid, kind, Surface::Cli).with_session(session)
    }

    #[test]
    fn uninitialized_dir_records_nothing() {
        let tmp = TempDir::new().unwrap();
        record_memory_events(tmp.path(), &[ev("s", "m", UsageKind::Fetched, None)]).unwrap();
        assert!(!tmp.path().join(".engramdb").exists());
    }

    #[test]
    fn one_call_writes_every_event_and_reads_back() {
        let tmp = project();
        let events = vec![
            ev("s", "m1", UsageKind::Returned, Some("a")),
            ev("s", "m2", UsageKind::Returned, Some("a")),
        ];
        record_memory_events(tmp.path(), &events).unwrap();
        let loaded = load_memory_events(tmp.path()).unwrap();
        assert_eq!(loaded.events, events);
        assert_eq!(loaded.skipped_lines, 0);
    }

    #[test]
    fn torn_lines_are_counted_not_fatal() {
        let tmp = project();
        record_memory_events(tmp.path(), &[ev("s", "m", UsageKind::Fetched, None)]).unwrap();
        let path = memory_events_path(tmp.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"id\":\"x\",\"ts\":");
        std::fs::write(&path, text).unwrap();
        record_memory_events(tmp.path(), &[ev("s", "m", UsageKind::Fetched, None)]).unwrap();
        let loaded = load_memory_events(tmp.path()).unwrap();
        assert_eq!((loaded.events.len(), loaded.skipped_lines), (2, 1));
    }

    #[test]
    fn rotation_bounds_the_log() {
        let tmp = project();
        for _ in 0..30 {
            record_at(tmp.path(), &[ev("s", "m", UsageKind::Fetched, None)], 600).unwrap();
        }
        let state = tmp.path().join(".engramdb/state");
        assert!(state.join(MEMORY_EVENTS_ROTATED_FILE).exists());
        assert!(load_memory_events(tmp.path()).unwrap().events.len() < 30);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_log_is_refused() {
        let tmp = project();
        let state = tmp.path().join(".engramdb/state");
        std::fs::create_dir_all(&state).unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, state.join(MEMORY_EVENTS_FILE)).unwrap();
        assert!(
            record_memory_events(tmp.path(), &[ev("s", "m", UsageKind::Fetched, None)]).is_err()
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(load_memory_events(tmp.path()).is_err());
    }

    #[tokio::test]
    async fn drain_moves_lines_once_and_combined_reads_never_double_count() {
        let (tmp, sid) = store_with_id();
        record_memory_events(
            tmp.path(),
            &[
                ev(&sid, "m1", UsageKind::Returned, Some("a")),
                ev(&sid, "m1", UsageKind::Injected, Some("a")),
            ],
        )
        .unwrap();
        let report = drain_with_grace(tmp.path(), &sid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!((report.events, report.files), (2, 1));
        assert!(!memory_events_path(tmp.path()).exists());

        // One more line, still in the JSONL, from a session the table saw.
        record_memory_events(
            tmp.path(),
            &[ev(&sid, "m1", UsageKind::Returned, Some("a"))],
        )
        .unwrap();
        let view = load_usage(tmp.path(), &sid, Some("m1")).await;
        let c = &view.by_memory["m1"].counts;
        assert_eq!((c.returned, c.injected), (2, 1));
        assert_eq!(c.returned_sessions, 1, "one session across table and log");

        // The rollup is as of the last drain.
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.returned, 1);
        let again = drain_with_grace(tmp.path(), &sid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(again.events, 1);
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.returned, 2);
        let after = load_usage(tmp.path(), &sid, Some("m1")).await;
        assert_eq!(
            after.by_memory["m1"].counts.returned, 2,
            "drained, not doubled"
        );
    }

    #[tokio::test]
    async fn a_redrain_after_a_crash_adds_nothing() {
        let (tmp, sid) = store_with_id();
        let events = vec![ev(&sid, "m1", UsageKind::Fetched, None)];
        record_memory_events(tmp.path(), &events).unwrap();
        // Inserted, then crashed before the delete: rows in the table, the
        // claimed file still on disk.
        ingest_memory_events(&sid, &events, true).await.unwrap();
        let state = tmp.path().join(".engramdb/state");
        std::fs::rename(
            state.join(MEMORY_EVENTS_FILE),
            state.join(format!("{MEMORY_EVENTS_DRAINING_PREFIX}crashed.0.jsonl")),
        )
        .unwrap();
        assert_eq!(
            load_usage(tmp.path(), &sid, None).await.by_memory["m1"]
                .counts
                .fetched,
            1,
            "a claimed file is invisible to reads"
        );
        let report = drain_with_grace(tmp.path(), &sid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 1);
    }

    #[tokio::test]
    async fn drain_without_a_store_data_dir_leaves_the_log_alone() {
        let tmp = project();
        record_memory_events(tmp.path(), &[ev("x", "m", UsageKind::Fetched, None)]).unwrap();
        let report = drain_with_grace(tmp.path(), "test-usage-ghost", Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report, DrainReport::default());
        assert!(memory_events_path(tmp.path()).exists());
    }

    #[tokio::test]
    async fn series_and_promotion_sessions_include_the_log() {
        let (tmp, sid) = store_with_id();
        let now = Utc::now();
        ingest_memory_events(
            &sid,
            &[ev(&sid, "m1", UsageKind::Returned, Some("a"))],
            false,
        )
        .await
        .unwrap();
        record_memory_events(
            tmp.path(),
            &[
                ev(&sid, "m1", UsageKind::Returned, Some("b")),
                ev(&sid, "m2", UsageKind::Returned, None),
            ],
        )
        .unwrap();
        let series = load_daily_series_at(tmp.path(), &sid, Some("m1"), 7, now).await;
        assert_eq!(series.len(), 7);
        assert_eq!(series.last().unwrap().returned, 2);
        let sessions = returned_sessions(tmp.path(), &sid, None).await;
        assert_eq!(sessions["m1"].len(), 2);
        assert!(
            !sessions.contains_key("m2"),
            "an anonymous return names no session"
        );
    }

    #[test]
    fn top_memories_sorts_by_one_signal_and_never_sums() {
        let mut view = UsageView::default();
        let mut put = |id: &str, returned, injected, fetched| {
            view.by_memory.insert(
                id.to_string(),
                MemoryUsage {
                    memory_id: id.to_string(),
                    store_id: "s".to_string(),
                    counts: UsageCounts {
                        returned,
                        injected,
                        fetched,
                        ..Default::default()
                    },
                },
            );
        };
        put("a", 100, 0, 1);
        put("b", 0, 0, 2);
        put("c", 0, 5, 1);
        put("d", 0, 0, 0);
        let ids = |sort| {
            top_memories(&view, 10, sort)
                .into_iter()
                .map(|u| u.memory_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(UsageSort::Fetched),
            ["b", "c", "a"],
            "unused memory left out"
        );
        assert_eq!(ids(UsageSort::Returned)[0], "a");
        assert_eq!(ids(UsageSort::Injected)[0], "c");
        assert_eq!(top_memories(&view, 1, UsageSort::Fetched).len(), 1);
    }
}
