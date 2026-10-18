//! Per-hook call counts and latency percentiles.
//!
//! The MCP server records tool calls through [`super::StatsCollector`], which
//! batches events in memory and flushes them to LanceDB on a timer. Hooks
//! cannot use that path: every `engramdb hook <event>` is its own short-lived
//! process, so there is no flush task, and one LanceDB append per hook would
//! cost milliseconds on every Read/Write/Edit and leave one table fragment per
//! call behind.
//!
//! So each hook appends one JSON line to `.engramdb/state/hook_stats.jsonl`
//! through [`crate::state_file`] — one `open` + `write`, no lock, the same
//! crash story as the harvest ledger (a torn write costs the torn line, which
//! [`load_hook_calls`] skips and counts). `state/` is gitignored, and the
//! lines hold only a hook name, a timestamp, a duration and an outcome.
//!
//! **Bounded by rotation, and the loss paths are declared:**
//! - When the live file passes [`ROTATE_BYTES`], the writer that noticed
//!   renames it to `hook_stats.1.jsonl`, replacing the previous generation.
//!   History is therefore at most two generations (~2 MiB, roughly 17,000
//!   calls); everything older is dropped by design, and the summary reports
//!   the oldest sample it saw as `since`.
//! - Two writers that both see the live file over the limit both rename. If
//!   a third process appended a fresh live file between the two renames, the
//!   second rename replaces the full generation with that near-empty file and
//!   one generation is lost. The window is the gap between two `rename(2)`
//!   calls; for a latency histogram that is an acceptable, declared loss.
//! - An append that races a rename lands in whichever file its descriptor
//!   points at; both files are read, so it is not lost.
//!
//! Percentiles are exact over every retained sample (nearest rank, the same
//! method as [`super::RingHistogram`]), not a recency window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::state_file::{append_state_file, read_state_file};
use crate::telemetry::RingHistogram;

/// Live log, under the project's `.engramdb/state/`.
pub const HOOK_STATS_FILE: &str = "hook_stats.jsonl";

/// Previous generation, kept after rotation.
pub const HOOK_STATS_ROTATED_FILE: &str = "hook_stats.1.jsonl";

/// Size at which the live file rotates.
pub const ROTATE_BYTES: u64 = 1024 * 1024;

/// One recorded hook invocation (one line of the log).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookCallRecord {
    pub ts: DateTime<Utc>,
    /// Hook subcommand name, e.g. `pre-tool-use`.
    pub hook: String,
    /// Wall time in milliseconds, from command entry to handler return.
    pub ms: f64,
    /// `false` when the handler returned an error (the hook still exited 0).
    pub ok: bool,
}

/// What [`load_hook_calls`] read back.
#[derive(Debug, Default)]
pub struct LoadedHookCalls {
    pub records: Vec<HookCallRecord>,
    /// Lines that did not parse — torn writes, or a format from another
    /// version. Reported rather than dropped silently.
    pub skipped_lines: u64,
}

/// Latency and outcome counts for one hook.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookTimingStats {
    pub calls: u64,
    pub errors: u64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Per-hook summary, keyed by hook name. Times are milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HookStatsView {
    /// Oldest sample included. Counts are "since this instant", not lifetime:
    /// rotation and `retention_days` both drop older samples.
    pub since: Option<DateTime<Utc>>,
    pub by_hook: BTreeMap<String, HookTimingStats>,
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped_lines: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl HookStatsView {
    pub fn is_empty(&self) -> bool {
        self.by_hook.is_empty()
    }
}

fn state_dir(project_root: &Path) -> PathBuf {
    crate::paths::project_dir(project_root).join("state")
}

/// Path of the live hook log for a project root.
pub fn hook_stats_path(project_root: &Path) -> PathBuf {
    state_dir(project_root).join(HOOK_STATS_FILE)
}

/// Append one hook call to the project's log, rotating when it is full.
///
/// Does nothing unless `project_root` holds an initialized store
/// (`.engramdb/manifest.toml` or `.engramdb/memories/`, the same test the
/// SessionEnd hook uses): the plugin registers hooks machine-wide, and every
/// directory Claude Code starts in would otherwise collect a state folder.
/// Callers treat an error as non-fatal.
pub fn record_hook_call(project_root: &Path, hook: &str, ms: f64, ok: bool) -> Result<()> {
    record_hook_call_at(project_root, hook, ms, ok, Utc::now(), ROTATE_BYTES)
}

fn record_hook_call_at(
    project_root: &Path,
    hook: &str,
    ms: f64,
    ok: bool,
    ts: DateTime<Utc>,
    rotate_bytes: u64,
) -> Result<()> {
    let engramdb = crate::paths::project_dir(project_root);
    if !engramdb.join("manifest.toml").is_file() && !engramdb.join("memories").is_dir() {
        return Ok(());
    }
    let record = HookCallRecord {
        ts,
        hook: hook.to_string(),
        // A NaN or negative duration cannot come from `Instant`, but keep the
        // log parseable (JSON has no NaN) whatever a caller passes.
        ms: if ms.is_finite() { ms.max(0.0) } else { 0.0 },
        ok,
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

/// Read both generations, oldest first. Missing files read as empty.
pub fn load_hook_calls(project_root: &Path) -> Result<LoadedHookCalls> {
    let dir = state_dir(project_root);
    let mut out = LoadedHookCalls::default();
    for name in [HOOK_STATS_ROTATED_FILE, HOOK_STATS_FILE] {
        let Some(text) = read_state_file(&dir.join(name))? else {
            continue;
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<HookCallRecord>(line) {
                Ok(r) => out.records.push(r),
                Err(_) => out.skipped_lines += 1,
            }
        }
    }
    Ok(out)
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
    let cutoff = retention_days.map(|d| now - chrono::Duration::days(d.min(36_500) as i64));
    let mut groups: BTreeMap<&str, Vec<&HookCallRecord>> = BTreeMap::new();
    let mut since: Option<DateTime<Utc>> = None;
    for r in &loaded.records {
        if cutoff.is_some_and(|c| r.ts < c) {
            continue;
        }
        since = Some(since.map_or(r.ts, |s| s.min(r.ts)));
        groups.entry(r.hook.as_str()).or_default().push(r);
    }
    let by_hook = groups
        .into_iter()
        .map(|(hook, rs)| {
            let mut hist = RingHistogram::new(rs.len());
            for r in &rs {
                hist.record(r.ms);
            }
            let stats = HookTimingStats {
                calls: rs.len() as u64,
                errors: rs.iter().filter(|r| !r.ok).count() as u64,
                avg: round1(hist.avg()),
                p50: round1(hist.percentile(0.50)),
                p95: round1(hist.percentile(0.95)),
                p99: round1(hist.percentile(0.99)),
            };
            (hook.to_string(), stats)
        })
        .collect();
    HookStatsView {
        since,
        by_hook,
        skipped_lines: loaded.skipped_lines,
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Load and summarize in one call. A read failure (for example a symlink
/// planted at the log path) is logged and reads as no data.
pub fn load_summary(project_root: &Path, retention_days: Option<u64>) -> HookStatsView {
    match load_hook_calls(project_root) {
        Ok(loaded) => summarize(&loaded, retention_days),
        Err(e) => {
            tracing::warn!("hook stats: could not read the hook log: {e}");
            HookStatsView::default()
        }
    }
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
            ts,
            hook: hook.to_string(),
            ms,
            ok,
        }
    }

    #[test]
    fn uninitialized_dir_records_nothing() {
        let tmp = TempDir::new().unwrap();
        record_hook_call(tmp.path(), "pre-tool-use", 5.0, true).unwrap();
        assert!(!tmp.path().join(".engramdb").exists());

        // A config file alone is not a store: still nothing recorded.
        std::fs::create_dir_all(tmp.path().join(".engramdb")).unwrap();
        std::fs::write(tmp.path().join(".engramdb/config.toml"), "").unwrap();
        record_hook_call(tmp.path(), "pre-tool-use", 5.0, true).unwrap();
        assert!(!tmp.path().join(".engramdb/state").exists());
    }

    #[test]
    fn record_then_load_round_trips() {
        let tmp = project();
        record_hook_call(tmp.path(), "pre-tool-use", 5.0, true).unwrap();
        record_hook_call(tmp.path(), "session-end", 12.5, false).unwrap();
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
        record_hook_call(tmp.path(), "pre-compact", f64::NAN, true).unwrap();
        record_hook_call(tmp.path(), "pre-compact", -3.0, true).unwrap();
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
            record_hook_call_at(tmp.path(), "pre-tool-use", i as f64, true, t, 200).unwrap();
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
        record_hook_call(tmp.path(), "pre-tool-use", 1.0, true).unwrap();
        let path = hook_stats_path(tmp.path());
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"ts\":\"2026-01-01T00:00:00Z\",\"hook\":\"pre-to");
        std::fs::write(&path, text).unwrap();
        // The next append starts on a fresh line, so the torn one costs one.
        record_hook_call(tmp.path(), "pre-tool-use", 2.0, true).unwrap();
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
        assert!(record_hook_call(tmp.path(), "pre-tool-use", 1.0, true).is_err());
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
}
