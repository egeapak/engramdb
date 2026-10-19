//! Per-hook call log for hooks that run in their own process.
//!
//! When the shared daemon executes a hook, the daemon records it straight
//! into the long-term `hook_events` table ([`super::hook_events`]). When a
//! hook runs in its own short-lived process instead (no daemon, an older
//! daemon, `--in-process`), that process has no flush task and must not pay
//! for a LanceDB write, so it appends one JSON line to
//! `.engramdb/state/hook_stats.jsonl` through [`crate::state_file`] — one
//! `open` + `write`, no lock, the same crash story as the harvest ledger (a
//! torn write costs the torn line, which [`load_hook_calls`] skips and
//! counts). `state/` is gitignored; a line holds an id, a hook name, a
//! timestamp, a duration, an outcome and the session id.
//!
//! A long-running process later moves those lines into `hook_events` with
//! [`drain_to_table`].
//!
//! **Bounded by rotation, and the loss paths are declared:**
//! - When the live file passes [`ROTATE_BYTES`], the writer that noticed
//!   renames it to `hook_stats.1.jsonl`, replacing the previous generation.
//!   Without a drainer, history is therefore at most two generations (~2 MiB,
//!   roughly 10,000 calls); anything older is dropped by design.
//! - Two writers that both see the live file over the limit both rename. If
//!   a third process appended a fresh live file between the two renames, the
//!   second rename replaces the full generation with that near-empty file and
//!   one generation is lost. The window is the gap between two `rename(2)`
//!   calls; for a latency histogram that is an acceptable, declared loss.
//! - A writer that opened the live file just before a drain renamed it writes
//!   into the renamed file. The drain waits [`DRAIN_GRACE`] after renaming
//!   before it reads, which covers an `open` + `write`; a writer stalled for
//!   longer than that between the two calls loses its line.
//! - `stats` never reads a file a drain has claimed, so nothing is counted
//!   twice. The other side of that: lines in a claimed file are missing from
//!   `stats` until a drain inserts them — for the length of a drain, or,
//!   after a drain that crashed before its delete, until the next drain.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::state_file::{append_state_file, read_state_file};
use crate::telemetry::hook_events::{
    append_hook_events, build_view, retention_cutoff, Groups, HookEvent, HookPath,
    PERCENTILE_SAMPLES,
};

pub use crate::telemetry::hook_events::{HookStatsView, HookTimingStats, PathTimingStats};

/// Live log, under the project's `.engramdb/state/`.
pub const HOOK_STATS_FILE: &str = "hook_stats.jsonl";

/// Previous generation, kept after rotation.
pub const HOOK_STATS_ROTATED_FILE: &str = "hook_stats.1.jsonl";

/// Prefix of a file a drain has claimed (renamed aside) and not yet deleted.
pub const HOOK_STATS_DRAINING_PREFIX: &str = "hook_stats.draining.";

/// Size at which the live file rotates.
pub const ROTATE_BYTES: u64 = 1024 * 1024;

/// How long a drain waits between claiming a file and reading it, so a
/// writer that opened the file just before the rename finishes its `write`.
pub const DRAIN_GRACE: Duration = Duration::from_millis(100);

/// One recorded hook invocation (one line of the log).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookCallRecord {
    /// Unique per call; the drain's dedupe key. Lines written before ids
    /// existed get one derived from their own bytes at load time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub ts: DateTime<Utc>,
    /// Hook subcommand name, e.g. `pre-tool-use`.
    pub hook: String,
    /// Wall time in milliseconds, from command entry to handler return.
    pub ms: f64,
    /// `false` when the handler returned an error (the hook still exited 0).
    pub ok: bool,
    /// Always `in_process` for lines this module writes; kept explicit so the
    /// format does not depend on that.
    #[serde(default)]
    pub path: HookPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl HookCallRecord {
    fn into_event(self) -> HookEvent {
        HookEvent {
            id: self.id,
            ts: self.ts,
            hook: self.hook,
            duration_ms: self.ms,
            ok: self.ok,
            path: self.path,
            session_id: self.session_id,
        }
    }
}

/// What [`load_hook_calls`] read back.
#[derive(Debug, Default)]
pub struct LoadedHookCalls {
    pub records: Vec<HookCallRecord>,
    /// Lines that did not parse — torn writes, or a format from another
    /// version. Reported rather than dropped silently.
    pub skipped_lines: u64,
}

fn state_dir(project_root: &Path) -> PathBuf {
    crate::paths::project_dir(project_root).join("state")
}

/// Path of the live hook log for a project root.
pub fn hook_stats_path(project_root: &Path) -> PathBuf {
    state_dir(project_root).join(HOOK_STATS_FILE)
}

/// Whether `project_root` holds an initialized store — the same test the
/// SessionEnd hook uses (`manifest.toml` or `memories/`).
fn is_initialized(project_root: &Path) -> bool {
    let engramdb = crate::paths::project_dir(project_root);
    engramdb.join("manifest.toml").is_file() || engramdb.join("memories").is_dir()
}

/// Append one hook call to the project's log, rotating when it is full.
///
/// Does nothing unless `project_root` holds an initialized store: the plugin
/// registers hooks machine-wide, and every directory Claude Code starts in
/// would otherwise collect a state folder. Callers treat an error as
/// non-fatal.
pub fn record_hook_call(
    project_root: &Path,
    hook: &str,
    ms: f64,
    ok: bool,
    session_id: Option<&str>,
) -> Result<()> {
    record_hook_call_at(
        project_root,
        hook,
        ms,
        ok,
        session_id,
        Utc::now(),
        ROTATE_BYTES,
    )
}

fn record_hook_call_at(
    project_root: &Path,
    hook: &str,
    ms: f64,
    ok: bool,
    session_id: Option<&str>,
    ts: DateTime<Utc>,
    rotate_bytes: u64,
) -> Result<()> {
    if !is_initialized(project_root) {
        return Ok(());
    }
    let record = HookCallRecord {
        id: uuid::Uuid::now_v7().to_string(),
        ts,
        hook: hook.to_string(),
        // A NaN or negative duration cannot come from `Instant`, but keep the
        // log parseable (JSON has no NaN) whatever a caller passes.
        ms: if ms.is_finite() { ms.max(0.0) } else { 0.0 },
        ok,
        path: HookPath::InProcess,
        session_id: session_id.filter(|s| !s.is_empty()).map(str::to_owned),
    };
    let mut line = serde_json::to_string(&record)?;
    line.push('\n');
    let live = hook_stats_path(project_root);
    append_state_file(&live, &line)?;

    // `symlink_metadata` reads no content; the append above already refused a
    // symlink at this path. `rename` never dereferences its source.
    if let Ok(meta) = std::fs::symlink_metadata(&live) {
        if meta.len() > rotate_bytes {
            let rotated = state_dir(project_root).join(HOOK_STATS_ROTATED_FILE);
            // ENOENT here means another writer rotated first (see module docs).
            let _ = std::fs::rename(&live, &rotated);
        }
    }
    Ok(())
}

fn parse_lines(text: &str, out: &mut LoadedHookCalls) {
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HookCallRecord>(line) {
            Ok(mut r) => {
                if r.id.is_empty() {
                    // Stable across re-reads, so a re-drain still dedupes.
                    r.id = format!("legacy-{}", crate::digest::hex_sha256(line.as_bytes()));
                }
                out.records.push(r);
            }
            Err(_) => out.skipped_lines += 1,
        }
    }
}

/// Read both generations, oldest first. Missing files read as empty. Files
/// a drain has claimed are not included (see the module docs).
pub fn load_hook_calls(project_root: &Path) -> Result<LoadedHookCalls> {
    let dir = state_dir(project_root);
    let mut out = LoadedHookCalls::default();
    for name in [HOOK_STATS_ROTATED_FILE, HOOK_STATS_FILE] {
        if let Some(text) = read_state_file(&dir.join(name))? {
            parse_lines(&text, &mut out);
        }
    }
    Ok(out)
}

/// Add records inside the retention window to `groups`.
pub fn add_records_to_groups(
    records: &[HookCallRecord],
    retention_days: Option<u64>,
    now: DateTime<Utc>,
    groups: &mut Groups,
) {
    let cutoff = retention_cutoff(retention_days, now);
    for r in records {
        if cutoff.is_some_and(|c| r.ts < c) {
            continue;
        }
        groups
            .entry((r.hook.clone(), r.path))
            .or_default()
            .add(r.ts, r.ms, r.ok);
    }
}

/// Summarize records per hook, ignoring samples older than `retention_days`.
pub fn summarize(loaded: &LoadedHookCalls, retention_days: Option<u64>) -> HookStatsView {
    summarize_at(loaded, retention_days, Utc::now())
}

fn summarize_at(
    loaded: &LoadedHookCalls,
    retention_days: Option<u64>,
    now: DateTime<Utc>,
) -> HookStatsView {
    let mut groups = Groups::new();
    add_records_to_groups(&loaded.records, retention_days, now, &mut groups);
    build_view(groups, PERCENTILE_SAMPLES, loaded.skipped_lines)
}

/// Load and summarize the JSONL log alone. A read failure (for example a
/// symlink planted at the log path) is logged and reads as no data.
pub fn load_summary(project_root: &Path, retention_days: Option<u64>) -> HookStatsView {
    match load_hook_calls(project_root) {
        Ok(loaded) => summarize(&loaded, retention_days),
        Err(e) => {
            tracing::warn!("hook stats: could not read the hook log: {e}");
            HookStatsView::default()
        }
    }
}

/// The full hook summary for a project: the `hook_events` table plus the
/// JSONL lines no drain has moved yet. Nothing is counted twice — a drain
/// claims a file (renames it) before it inserts, and claimed files are not
/// read here. A failure on either side is logged and that side reads empty.
pub async fn load_combined_summary(
    project_root: &Path,
    project_id: &str,
    retention_days: Option<u64>,
) -> HookStatsView {
    let mut groups = match crate::telemetry::hook_events::load_groups(
        project_id,
        retention_days,
        PERCENTILE_SAMPLES,
    )
    .await
    {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!("hook stats: could not read hook_events: {e}");
            Groups::new()
        }
    };
    let loaded = match load_hook_calls(project_root) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("hook stats: could not read the hook log: {e}");
            LoadedHookCalls::default()
        }
    };
    add_records_to_groups(&loaded.records, retention_days, Utc::now(), &mut groups);
    build_view(groups, PERCENTILE_SAMPLES, loaded.skipped_lines)
}

/// What one [`drain_to_table`] call moved.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Records handed to the table (already-known ids are ignored there).
    pub records: u64,
    /// Lines that did not parse and were dropped with their file.
    pub skipped_lines: u64,
    /// Files claimed and deleted.
    pub files: u64,
}

/// Move the project's JSONL hook log into the `hook_events` table.
///
/// 1. Claim: rename the rotated and live files to
///    `hook_stats.draining.<pid>.<n>.jsonl`. New hooks then write a fresh live
///    file. Files left by a drain that crashed are picked up too.
/// 2. Wait [`DRAIN_GRACE`], then read every claimed file.
/// 3. Insert with `merge_insert` on `id`.
/// 4. Delete the claimed files — only after the insert succeeded. A crash or
///    an insert error leaves them for the next drain; the ids make the
///    re-insert harmless.
///
/// Serialized per project by a lock file in the global data dir (never under
/// `.engramdb/state/`, whose files only `state_file` may open). Unparseable
/// lines are counted in the report and deleted with their file: no later
/// drain could read them either.
pub async fn drain_to_table(project_root: &Path, project_id: &str) -> anyhow::Result<DrainReport> {
    drain_to_table_with_grace(project_root, project_id, DRAIN_GRACE).await
}

async fn drain_to_table_with_grace(
    project_root: &Path,
    project_id: &str,
    grace: Duration,
) -> anyhow::Result<DrainReport> {
    let mut report = DrainReport::default();
    if !is_initialized(project_root) {
        return Ok(report);
    }
    // The lock file must not be what creates `projects/<id>/`: `hook_events`
    // refuses to create scaffolding for a project whose global dir is
    // missing, and a lock created first would defeat that check.
    let project_data = crate::paths::global_data_dir()?
        .join("projects")
        .join(project_id);
    if !project_data.is_dir() {
        return Ok(report);
    }
    let lock_path = project_data.join("hook_drain.lock");
    let _guard = crate::write_lock::acquire_lock_file(lock_path).await?;

    let dir = state_dir(project_root);
    let stamp = format!(
        "{}.{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let mut claimed_new = false;
    for (i, name) in [HOOK_STATS_ROTATED_FILE, HOOK_STATS_FILE]
        .iter()
        .enumerate()
    {
        let target = dir.join(format!("{HOOK_STATS_DRAINING_PREFIX}{stamp}.{i}.jsonl"));
        match std::fs::rename(dir.join(name), &target) {
            Ok(()) => claimed_new = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut claimed: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(HOOK_STATS_DRAINING_PREFIX))
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    if claimed.is_empty() {
        return Ok(report);
    }
    claimed.sort();
    if claimed_new {
        tokio::time::sleep(grace).await;
    }

    let mut loaded = LoadedHookCalls::default();
    for path in &claimed {
        if let Some(text) = read_state_file(path)? {
            parse_lines(&text, &mut loaded);
        }
    }
    report.records = loaded.records.len() as u64;
    report.skipped_lines = loaded.skipped_lines;
    let events: Vec<HookEvent> = loaded
        .records
        .into_iter()
        .map(HookCallRecord::into_event)
        .collect();
    append_hook_events(project_id, &events, true).await?;
    for path in &claimed {
        match std::fs::remove_file(path) {
            Ok(()) => report.files += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("hook stats: could not delete {}: {e}", path.display()),
        }
    }
    if report.skipped_lines > 0 {
        tracing::warn!(
            "hook stats: dropped {} unreadable line(s) while draining {}",
            report.skipped_lines,
            dir.display()
        );
    }
    Ok(report)
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

    fn rec(hook: &str, ms: f64, ok: bool, ts: DateTime<Utc>) -> HookCallRecord {
        HookCallRecord {
            id: format!("{hook}-{ms}-{}", ts.timestamp_micros()),
            ts,
            hook: hook.to_string(),
            ms,
            ok,
            path: HookPath::InProcess,
            session_id: None,
        }
    }

    #[test]
    fn uninitialized_dir_records_nothing() {
        let tmp = TempDir::new().unwrap();
        record_hook_call(tmp.path(), "pre-tool-use", 5.0, true, None).unwrap();
        assert!(!tmp.path().join(".engramdb").exists());

        // A config file alone is not a store: still nothing recorded.
        std::fs::create_dir_all(tmp.path().join(".engramdb")).unwrap();
        std::fs::write(tmp.path().join(".engramdb/config.toml"), "").unwrap();
        record_hook_call(tmp.path(), "pre-tool-use", 5.0, true, None).unwrap();
        assert!(!tmp.path().join(".engramdb/state").exists());
    }

    #[test]
    fn record_then_load_round_trips() {
        let tmp = project();
        record_hook_call(tmp.path(), "pre-tool-use", 5.0, true, None).unwrap();
        record_hook_call(tmp.path(), "session-end", 12.5, false, None).unwrap();
        let loaded = load_hook_calls(tmp.path()).unwrap();
        assert_eq!(loaded.records.len(), 2);
        assert_eq!(loaded.skipped_lines, 0);
        assert_eq!(loaded.records[0].hook, "pre-tool-use");
        assert_eq!(loaded.records[1].ms, 12.5);
        assert!(!loaded.records[1].ok);
    }

    #[test]
    fn non_finite_duration_stays_parseable() {
        let tmp = project();
        record_hook_call(tmp.path(), "pre-compact", f64::NAN, true, None).unwrap();
        record_hook_call(tmp.path(), "pre-compact", -3.0, true, None).unwrap();
        let loaded = load_hook_calls(tmp.path()).unwrap();
        assert_eq!(loaded.skipped_lines, 0);
        assert!(loaded.records.iter().all(|r| r.ms == 0.0));
    }

    #[test]
    fn rotation_keeps_both_generations_and_drops_the_third() {
        let tmp = project();
        let t = Utc::now();
        // Each line is ~80 bytes, so a 200-byte limit rotates every few calls.
        for i in 0..20 {
            record_hook_call_at(tmp.path(), "pre-tool-use", i as f64, true, None, t, 200).unwrap();
        }
        let state = tmp.path().join(".engramdb/state");
        assert!(state.join(HOOK_STATS_ROTATED_FILE).exists());
        let live_len = std::fs::metadata(state.join(HOOK_STATS_FILE))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(live_len <= 200, "live file must rotate past the limit");
        let loaded = load_hook_calls(tmp.path()).unwrap();
        assert!(loaded.records.len() < 20, "older generations are dropped");
        // The newest sample always survives.
        assert_eq!(loaded.records.last().map(|r| r.ms), Some(19.0));
        // Oldest first: the rotated generation is read before the live one.
        let ms: Vec<f64> = loaded.records.iter().map(|r| r.ms).collect();
        assert!(ms.windows(2).all(|w| w[0] < w[1]), "{ms:?}");
    }

    #[test]
    fn torn_and_foreign_lines_are_counted_not_fatal() {
        let tmp = project();
        record_hook_call(tmp.path(), "pre-tool-use", 1.0, true, None).unwrap();
        let path = hook_stats_path(tmp.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"ts\":\"2026-01-01T00:00:00Z\",\"hook\":\"pre-to");
        std::fs::write(&path, text).unwrap();
        // The next append starts on a fresh line, so the torn one costs one.
        record_hook_call(tmp.path(), "pre-tool-use", 2.0, true, None).unwrap();
        let loaded = load_hook_calls(tmp.path()).unwrap();
        assert_eq!(loaded.records.len(), 2);
        assert_eq!(loaded.skipped_lines, 1);
        assert_eq!(summarize(&loaded, None).skipped_lines, 1);
    }

    #[test]
    fn missing_log_summarizes_to_empty() {
        let tmp = project();
        let view = load_summary(tmp.path(), Some(90));
        assert!(view.is_empty());
        assert_eq!(view.since, None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_log_is_refused_on_write_and_read() {
        let tmp = project();
        let state = tmp.path().join(".engramdb/state");
        std::fs::create_dir_all(&state).unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, state.join(HOOK_STATS_FILE)).unwrap();
        assert!(record_hook_call(tmp.path(), "pre-tool-use", 1.0, true, None).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(load_hook_calls(tmp.path()).is_err());
        assert!(load_summary(tmp.path(), None).is_empty());
    }

    #[test]
    fn percentiles_are_nearest_rank_per_hook() {
        let now = Utc::now();
        let mut records: Vec<HookCallRecord> = (1..=100)
            .map(|i| rec("pre-tool-use", i as f64, true, now))
            .collect();
        records.push(rec("session-end", 40.0, false, now));
        records.push(rec("session-end", 60.0, true, now));
        let loaded = LoadedHookCalls {
            records,
            skipped_lines: 0,
        };
        let view = summarize_at(&loaded, None, now);
        let p = &view.by_hook["pre-tool-use"];
        assert_eq!(p.calls, 100);
        assert_eq!(p.errors, 0);
        assert_eq!(p.p50, 50.0);
        assert_eq!(p.p95, 95.0);
        assert_eq!(p.p99, 99.0);
        assert_eq!(p.avg, 50.5);
        let s = &view.by_hook["session-end"];
        assert_eq!(s.calls, 2);
        assert_eq!(s.errors, 1);
        assert_eq!(s.p50, 40.0);
        assert_eq!(s.p99, 60.0);
        assert_eq!(view.since, Some(now));
    }

    #[test]
    fn retention_drops_old_samples_and_moves_since() {
        let now = Utc::now();
        let old = now - chrono::Duration::days(100);
        let recent = now - chrono::Duration::days(1);
        let loaded = LoadedHookCalls {
            records: vec![
                rec("pre-tool-use", 900.0, true, old),
                rec("pre-tool-use", 10.0, true, recent),
                rec("pre-compact", 5.0, true, old),
            ],
            skipped_lines: 0,
        };
        let view = summarize_at(&loaded, Some(90), now);
        assert_eq!(
            view.by_hook.len(),
            1,
            "pre-compact is entirely out of window"
        );
        assert_eq!(view.by_hook["pre-tool-use"].calls, 1);
        assert_eq!(view.by_hook["pre-tool-use"].p99, 10.0);
        assert_eq!(view.since, Some(recent));

        let unlimited = summarize_at(&loaded, None, now);
        assert_eq!(unlimited.by_hook["pre-tool-use"].calls, 2);
        assert_eq!(unlimited.since, Some(old));
    }

    #[test]
    fn records_carry_unique_ids_and_the_session() {
        let tmp = project();
        record_hook_call(tmp.path(), "pre-tool-use", 1.0, true, Some("sess-1")).unwrap();
        record_hook_call(tmp.path(), "pre-tool-use", 2.0, true, Some("")).unwrap();
        let loaded = load_hook_calls(tmp.path()).unwrap();
        assert_eq!(loaded.records.len(), 2);
        assert_ne!(loaded.records[0].id, loaded.records[1].id);
        assert!(!loaded.records[0].id.is_empty());
        assert_eq!(loaded.records[0].session_id.as_deref(), Some("sess-1"));
        assert_eq!(
            loaded.records[1].session_id, None,
            "empty session ids are dropped"
        );
        assert!(loaded.records.iter().all(|r| r.path == HookPath::InProcess));
    }

    #[test]
    fn legacy_lines_without_an_id_get_a_stable_one() {
        let tmp = project();
        let path = hook_stats_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"ts\":\"2026-10-18T10:00:00Z\",\"hook\":\"pre-compact\",\"ms\":1.5,\"ok\":true}\n",
        )
        .unwrap();
        let a = load_hook_calls(tmp.path()).unwrap();
        let b = load_hook_calls(tmp.path()).unwrap();
        assert_eq!(a.records.len(), 1);
        assert!(a.records[0].id.starts_with("legacy-"));
        assert_eq!(a.records[0].id, b.records[0].id);
        assert_eq!(a.records[0].path, HookPath::InProcess);
    }

    /// A project whose global data dir exists, as `MemoryStore::init` leaves it.
    fn project_with_id() -> (TempDir, String) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let pid = format!("test-drain-{:016x}", N.fetch_add(1, Ordering::Relaxed));
        let data = crate::paths::global_data_dir()
            .unwrap()
            .join("projects")
            .join(&pid);
        std::fs::create_dir_all(&data).unwrap();
        (project(), pid)
    }

    async fn table_calls(pid: &str) -> u64 {
        crate::telemetry::hook_events::load_groups(pid, None, PERCENTILE_SAMPLES)
            .await
            .unwrap()
            .values()
            .map(|g| g.calls)
            .sum()
    }

    #[tokio::test]
    async fn drain_moves_every_line_into_the_table_once() {
        let (tmp, pid) = project_with_id();
        for i in 0..3 {
            record_hook_call(tmp.path(), "pre-tool-use", i as f64, true, Some("s")).unwrap();
        }
        let report = drain_to_table_with_grace(tmp.path(), &pid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report.records, 3);
        assert_eq!(report.files, 1);
        assert!(!hook_stats_path(tmp.path()).exists());
        assert_eq!(table_calls(&pid).await, 3);
        assert!(
            load_summary(tmp.path(), None).is_empty(),
            "the file is gone"
        );

        // Nothing left: a second drain is a no-op.
        let again = drain_to_table_with_grace(tmp.path(), &pid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(again, DrainReport::default());
        assert_eq!(table_calls(&pid).await, 3);
    }

    #[tokio::test]
    async fn a_redrain_after_a_crash_adds_nothing() {
        let (tmp, pid) = project_with_id();
        record_hook_call(tmp.path(), "pre-compact", 1.0, true, None).unwrap();
        record_hook_call(tmp.path(), "pre-compact", 2.0, true, None).unwrap();
        // Simulate a drain that inserted, then crashed before its delete: the
        // rows are in the table and the claimed file is still on disk.
        let loaded = load_hook_calls(tmp.path()).unwrap();
        let events: Vec<HookEvent> = loaded
            .records
            .into_iter()
            .map(HookCallRecord::into_event)
            .collect();
        append_hook_events(&pid, &events, true).await.unwrap();
        let state = tmp.path().join(".engramdb/state");
        std::fs::rename(
            state.join(HOOK_STATS_FILE),
            state.join(format!("{HOOK_STATS_DRAINING_PREFIX}crashed.0.jsonl")),
        )
        .unwrap();
        // The claimed file is invisible to stats, and not double counted.
        assert!(load_summary(tmp.path(), None).is_empty());

        let report = drain_to_table_with_grace(tmp.path(), &pid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report.records, 2);
        assert_eq!(report.files, 1);
        assert_eq!(
            table_calls(&pid).await,
            2,
            "ids made the re-insert harmless"
        );
    }

    #[tokio::test]
    async fn drain_claims_both_generations_and_keeps_new_writes_apart() {
        let (tmp, pid) = project_with_id();
        let t = Utc::now();
        for i in 0..12 {
            record_hook_call_at(tmp.path(), "pre-tool-use", i as f64, true, None, t, 400).unwrap();
        }
        let state = tmp.path().join(".engramdb/state");
        assert!(state.join(HOOK_STATS_ROTATED_FILE).exists());
        let before = load_hook_calls(tmp.path()).unwrap().records.len() as u64;
        let report = drain_to_table_with_grace(tmp.path(), &pid, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report.records, before);
        assert!(!state.join(HOOK_STATS_ROTATED_FILE).exists());
        // A write after the drain lands in a fresh live file.
        record_hook_call(tmp.path(), "pre-tool-use", 99.0, true, None).unwrap();
        assert_eq!(load_hook_calls(tmp.path()).unwrap().records.len(), 1);
        assert_eq!(table_calls(&pid).await, before);
    }

    #[tokio::test]
    async fn drain_without_a_project_data_dir_leaves_the_log_alone() {
        let tmp = project();
        record_hook_call(tmp.path(), "pre-compact", 1.0, true, None).unwrap();
        let report = drain_to_table_with_grace(tmp.path(), "test-drain-ghost", Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(report, DrainReport::default());
        assert!(hook_stats_path(tmp.path()).exists());
        assert!(!crate::paths::global_data_dir()
            .unwrap()
            .join("projects/test-drain-ghost")
            .exists());
    }

    #[tokio::test]
    async fn combined_summary_reads_table_and_undrained_lines() {
        let (tmp, pid) = project_with_id();
        record_hook_call(tmp.path(), "pre-tool-use", 10.0, true, None).unwrap();
        drain_to_table_with_grace(tmp.path(), &pid, Duration::ZERO)
            .await
            .unwrap();
        record_hook_call(tmp.path(), "pre-tool-use", 30.0, false, None).unwrap();
        let daemon_row = HookEvent {
            id: "d1".to_string(),
            ts: Utc::now(),
            hook: "pre-tool-use".to_string(),
            duration_ms: 5.0,
            ok: true,
            path: HookPath::Daemon,
            session_id: None,
        };
        append_hook_events(&pid, &[daemon_row], false)
            .await
            .unwrap();

        let view = load_combined_summary(tmp.path(), &pid, Some(90)).await;
        let h = &view.by_hook["pre-tool-use"];
        assert_eq!((h.calls, h.errors), (3, 1));
        assert_eq!(h.by_path["in_process"].calls, 2, "one drained, one pending");
        assert_eq!(h.by_path["daemon"].calls, 1);
        assert_eq!(h.p99, 30.0);
    }
}
