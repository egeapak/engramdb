//! Claude Code hooks executed by the daemon.
//!
//! When a daemon is running, `engramdb hook <event>` sends the event to it
//! instead of doing the work in its own short-lived process. The daemon keeps
//! each project's store warm, so a hook skips the store open and the cold
//! LanceDB reads, and `user-prompt-submit` uses the daemon's loaded embedding
//! model directly.
//!
//! **The handlers live in the CLI crate**, which depends on this one, so the
//! daemon cannot call them. `engramdb daemon run` injects them as a
//! [`HookExecutor`] — the same inversion `doctor` uses for `check_daemon`.
//!
//! **This module also owns the daemon's hook telemetry.** Each served hook is
//! buffered per project and written to the project's `hook_events` table: on
//! a [`HOOK_FLUSH_INTERVAL`] timer, as soon as one project reaches
//! [`HOOK_FLUSH_BATCH`] events, and on every shutdown path. A daemon killed
//! with `SIGKILL` loses at most one interval of buffered events — the same
//! declared loss the MCP server's `stats_events` flush task has.
//!
//! After answering, the daemon also runs throttled background maintenance for
//! the project ([`maintain`]): it drains the project's hook JSONL (lines
//! written by hooks that ran in-process) into `hook_events`, and runs the
//! existing cheap fragment-count compaction check, so hooks keep reading a
//! compact table.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;

use super::protocol::HookCall;
use crate::ops::ProviderCache;
use crate::telemetry::hook_events::{
    append_hook_events, optimize_hook_events, prune_hook_events, HookEvent, HookPath,
};

/// How often buffered hook events are written to `hook_events`.
pub const HOOK_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// A project's buffer is written as soon as it holds this many events.
pub const HOOK_FLUSH_BATCH: usize = 1_024;

/// Retention prune and table compaction run on every this-many-th flush, for
/// the projects that flush wrote.
const PRUNE_EVERY_FLUSHES: u64 = 10;

/// Background maintenance runs at most this often per project.
pub const HOOK_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// The project a hook ran for, as resolved by the executor (worktree routing
/// applied), plus the settings telemetry needs.
#[derive(Debug, Clone, PartialEq)]
pub struct HookProject {
    /// Project root after worktree resolution.
    pub root: PathBuf,
    pub project_id: String,
    /// `[stats].enabled`.
    pub stats_enabled: bool,
    /// `[stats].retention_days`.
    pub retention_days: Option<u64>,
    /// From the hook event, when it carried one.
    pub session_id: Option<String>,
}

/// What a hook run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct HookRun {
    /// What the hook prints (the hook protocol document), if anything.
    pub stdout: Option<String>,
    /// `false` when the handler returned an error. The client still exits 0.
    pub ok: bool,
    /// `None` when the directory is not an initialized project; nothing is
    /// recorded or maintained then.
    pub project: Option<HookProject>,
}

/// Runs hook handlers inside the daemon. Implemented by the CLI crate.
pub trait HookExecutor: Send + Sync {
    /// Run `call` for the directory the client was invoked in. `dir` is
    /// absolute (the client resolves it against its own working directory;
    /// the daemon's is unrelated). `models` is the daemon's provider cache,
    /// for hooks that need a model.
    fn run<'a>(
        &'a self,
        call: &'a HookCall,
        dir: &'a Path,
        stdin: &'a str,
        models: &'a ProviderCache,
    ) -> Pin<Box<dyn Future<Output = HookRun> + Send + 'a>>;
}

/// The daemon's hook telemetry state.
#[derive(Default)]
pub(crate) struct HookTelemetry {
    buf: Mutex<HashMap<String, Vec<HookEvent>>>,
    retention: Mutex<HashMap<String, Option<u64>>>,
    last_maintenance: Mutex<HashMap<PathBuf, Instant>>,
    flushes: AtomicU64,
}

impl HookTelemetry {
    /// Buffer one served hook. Returns a batch to write right away when the
    /// project's buffer is full.
    pub(crate) fn record(
        &self,
        call: &HookCall,
        project: &HookProject,
        duration_ms: f64,
        ok: bool,
    ) -> Option<(String, Vec<HookEvent>)> {
        if !project.stats_enabled {
            return None;
        }
        let event = HookEvent {
            id: uuid::Uuid::now_v7().to_string(),
            ts: Utc::now(),
            hook: call.name().to_string(),
            duration_ms: if duration_ms.is_finite() {
                duration_ms.max(0.0)
            } else {
                0.0
            },
            ok,
            path: HookPath::Daemon,
            session_id: project.session_id.clone(),
        };
        if let Ok(mut r) = self.retention.lock() {
            r.insert(project.project_id.clone(), project.retention_days);
        }
        let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        let entry = buf.entry(project.project_id.clone()).or_default();
        entry.push(event);
        if entry.len() >= HOOK_FLUSH_BATCH {
            let batch = std::mem::take(entry);
            buf.remove(&project.project_id);
            return Some((project.project_id.clone(), batch));
        }
        None
    }

    /// Write every buffered event. Failures are logged and the events
    /// dropped: telemetry must never wedge the daemon.
    pub(crate) async fn flush(&self) {
        let drained: Vec<(String, Vec<HookEvent>)> = {
            let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
            buf.drain().collect()
        };
        let maintain =
            (self.flushes.fetch_add(1, Ordering::Relaxed) + 1).is_multiple_of(PRUNE_EVERY_FLUSHES);
        for (pid, events) in drained {
            write_batch(&pid, &events).await;
            if maintain {
                let retention = self
                    .retention
                    .lock()
                    .ok()
                    .and_then(|r| r.get(&pid).copied())
                    .flatten();
                if let Some(days) = retention {
                    if let Err(e) = prune_hook_events(&pid, days).await {
                        tracing::warn!("hook_events prune for {pid}: {e}");
                    }
                }
                if let Err(e) = optimize_hook_events(&pid).await {
                    tracing::debug!("hook_events optimize for {pid}: {e}");
                }
            }
        }
    }

    /// Whether background maintenance is due for `root`; marks it as run.
    pub(crate) fn maintenance_due(&self, root: &Path) -> bool {
        let mut last = self
            .last_maintenance
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match last.get(root) {
            Some(t) if now.duration_since(*t) < HOOK_MAINTENANCE_INTERVAL => false,
            _ => {
                last.insert(root.to_path_buf(), now);
                true
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn buffered(&self, project_id: &str) -> usize {
        self.buf.lock().unwrap().get(project_id).map_or(0, Vec::len)
    }
}

pub(crate) async fn write_batch(project_id: &str, events: &[HookEvent]) {
    if let Err(e) = append_hook_events(project_id, events, false).await {
        tracing::warn!(
            "hook_events: failed to write {} event(s) for project {project_id}: {e}",
            events.len()
        );
    }
}

/// Spawn the periodic flush. Exits when `shutdown` fires; the shutdown paths
/// flush on their own.
pub(crate) fn spawn_flush_task(
    telemetry: Arc<HookTelemetry>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(HOOK_FLUSH_INTERVAL) => telemetry.flush().await,
                _ = shutdown.changed() => return,
            }
        }
    });
}

/// Background maintenance for a project the daemon just served: drain its
/// hook JSONL into `hook_events`, then the cheap compaction check. Both are
/// best-effort.
pub(crate) async fn maintain(project: HookProject) {
    if project.stats_enabled {
        match crate::telemetry::hooks::drain_to_table(&project.root, &project.project_id).await {
            Ok(r) if r.records > 0 => {
                tracing::debug!(
                    "hook stats: drained {} record(s) into hook_events",
                    r.records
                )
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("hook stats: drain failed (non-fatal): {e}"),
        }
    }
    let config_path = project.root.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;
    crate::ops::auto_compact(&project.root, &config.maintenance, false).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(stats_enabled: bool) -> HookProject {
        HookProject {
            root: PathBuf::from("/nonexistent/project"),
            project_id: "test-daemon-hooks".to_string(),
            stats_enabled,
            retention_days: Some(90),
            session_id: Some("s".to_string()),
        }
    }

    #[test]
    fn record_buffers_and_hands_back_a_full_batch() {
        let t = HookTelemetry::default();
        let p = project(true);
        for _ in 0..HOOK_FLUSH_BATCH - 1 {
            assert!(t.record(&HookCall::PreToolUse, &p, 1.0, true).is_none());
        }
        assert_eq!(t.buffered(&p.project_id), HOOK_FLUSH_BATCH - 1);
        let (pid, batch) = t
            .record(&HookCall::PreToolUse, &p, f64::NAN, false)
            .expect("the 1024th event fills the batch");
        assert_eq!(pid, p.project_id);
        assert_eq!(batch.len(), HOOK_FLUSH_BATCH);
        assert_eq!(t.buffered(&p.project_id), 0);
        let last = batch.last().unwrap();
        assert_eq!(last.duration_ms, 0.0, "non-finite durations are clamped");
        assert_eq!(last.path, HookPath::Daemon);
        assert_eq!(last.hook, "pre-tool-use");
        assert!(!last.ok);
        assert_eq!(last.session_id.as_deref(), Some("s"));
    }

    #[test]
    fn record_skips_projects_with_stats_disabled() {
        let t = HookTelemetry::default();
        assert!(t
            .record(&HookCall::PreCompact, &project(false), 1.0, true)
            .is_none());
        assert_eq!(t.buffered("test-daemon-hooks"), 0);
    }

    #[test]
    fn maintenance_is_throttled_per_project() {
        let t = HookTelemetry::default();
        assert!(t.maintenance_due(Path::new("/a")));
        assert!(!t.maintenance_due(Path::new("/a")));
        assert!(t.maintenance_due(Path::new("/b")));
    }
}
