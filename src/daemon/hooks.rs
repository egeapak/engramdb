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
//! The same buffer carries the hooks' per-memory `injected` usage events
//! ([`HookRun::usage_events`]), keyed by the store that owns them, into that
//! store's `memory_events` table and `memory_usage` rollup
//! ([`crate::telemetry::memory_events`]) — on the same timer, batch size and
//! shutdown paths, with the same `SIGKILL` loss. A hook in its own process
//! appends them to the JSONL log instead.
//!
//! After answering, the daemon also runs throttled background maintenance for
//! the project ([`maintain`]): it drains the project's hook JSONL (lines
//! written by hooks that ran in-process) into `hook_events` and its usage
//! JSONL (and that of the shared stores it reads) into `memory_events`, and
//! runs the existing cheap fragment-count compaction check, so hooks keep
//! reading a compact table. The usage rollup reconcile and the usage-table
//! compaction are heavier (a full scan under the rollup lock, a Lance
//! compaction) and every project reaches the global store, so they run at
//! most once per [`HOOK_PRUNE_INTERVAL`] per **store**, not per pass.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;

use super::protocol::HookCall;
use crate::ops::ProviderCache;
use crate::telemetry::hook_events::{
    append_hook_events, optimize_hook_events, prune_hook_events, HookEvent, HookPath,
};
use crate::telemetry::memory_events::{
    ingest_memory_events, optimize_memory_events, prune_memory_events, reconcile_rollup,
    MemoryEvent,
};

/// How often buffered hook events are written to `hook_events`.
pub const HOOK_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// A project's buffer is written as soon as it holds this many events.
pub const HOOK_FLUSH_BATCH: usize = 1_024;

/// Retention prune and table compaction run for a project at most this
/// often, on a flush that writes that project's events.
const HOOK_PRUNE_INTERVAL: Duration = Duration::from_secs(600);

/// How many await-and-drain rounds one [`HookTelemetry::flush`] runs at most.
const FLUSH_ROUNDS: usize = 4;

/// Background maintenance runs at most this often per project.
pub const HOOK_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// Environment variables that change where a hook reads and writes, or
/// which model it may load. A hook runs with the daemon's values, so the
/// daemon runs a hook only when the client's values are the same; otherwise
/// the client runs it in-process. Without this, a session started with its
/// own `ENGRAMDB_DATA_DIR` would have its transcript copies, registry entries
/// and hook events written to the data dir of the session that spawned the
/// daemon.
pub const HOOK_ENV_KEYS: &[&str] = &[
    "ENGRAMDB_DATA_DIR",
    "ENGRAMDB_CONFIG_DIR",
    "ENGRAMDB_REGISTRY_PATH",
    "ENGRAMDB_MODEL_CACHE_DIR",
    "ENGRAMDB_OFFLINE",
    "ENGRAMDB_EMBEDDING_BACKEND",
    "CLAUDE_CONFIG_DIR",
    "HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
];

/// This process's value of each [`HOOK_ENV_KEYS`] variable that is set.
pub fn hook_env() -> std::collections::BTreeMap<String, String> {
    HOOK_ENV_KEYS
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect()
}

/// The first [`HOOK_ENV_KEYS`] variable whose value differs between
/// `client` and `daemon`, if any.
pub(crate) fn env_mismatch(
    client: &std::collections::BTreeMap<String, String>,
    daemon: &std::collections::BTreeMap<String, String>,
) -> Option<&'static str> {
    HOOK_ENV_KEYS
        .iter()
        .copied()
        .find(|k| client.get(*k) != daemon.get(*k))
}

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
    /// Per-memory `injected` events the hook produced, for the daemon to
    /// buffer (already gated on the project's `[stats].enabled`).
    pub usage_events: Vec<MemoryEvent>,
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
    /// Usage events, keyed by owning store id.
    usage: Mutex<HashMap<String, Vec<MemoryEvent>>>,
    retention: Mutex<HashMap<String, Option<u64>>>,
    last_maintenance: Mutex<HashMap<PathBuf, Instant>>,
    /// When each project's `hook_events` was last pruned and compacted.
    last_prune: Mutex<HashMap<String, Instant>>,
    /// Full-batch writes started by [`Self::spawn_write`], awaited by
    /// [`Self::flush`] so a shutdown does not exit before they commit.
    writes: Mutex<Vec<tokio::task::JoinHandle<()>>>,
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

    /// Buffer a hook's usage events per owning store. Returns the batches to
    /// write right away when a store's buffer is full.
    pub(crate) fn record_usage(
        &self,
        project: &HookProject,
        events: Vec<MemoryEvent>,
    ) -> Vec<(String, Vec<MemoryEvent>)> {
        if !project.stats_enabled || events.is_empty() {
            return Vec::new();
        }
        if let Ok(mut r) = self.retention.lock() {
            for e in &events {
                r.insert(e.store_id.clone(), project.retention_days);
            }
        }
        let mut buf = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        let mut full = Vec::new();
        for e in events {
            let store = e.store_id.clone();
            let entry = buf.entry(store.clone()).or_default();
            entry.push(e);
            if entry.len() >= HOOK_FLUSH_BATCH {
                let batch = std::mem::take(entry);
                buf.remove(&store);
                full.push((store, batch));
            }
        }
        full
    }

    /// Write a full usage batch from [`Self::record_usage`] in the
    /// background.
    pub(crate) fn spawn_usage_write(&self, store_id: String, batch: Vec<MemoryEvent>) {
        let handle = tokio::spawn(async move { write_usage_batch(&store_id, &batch).await });
        let mut writes = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        writes.retain(|h| !h.is_finished());
        writes.push(handle);
    }

    /// Write a full batch returned by [`Self::record`] in the background,
    /// without delaying the hook's reply.
    pub(crate) fn spawn_write(&self, project_id: String, batch: Vec<HookEvent>) {
        let handle = tokio::spawn(async move { write_batch(&project_id, &batch).await });
        let mut writes = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        writes.retain(|h| !h.is_finished());
        writes.push(handle);
    }

    /// Write every buffered event, after waiting for the background batch
    /// writes. Failures are logged and the events dropped: telemetry must
    /// never wedge the daemon.
    ///
    /// Hooks can still be served while a flush runs (a shutdown flush
    /// included), and one of them can fill a batch and spawn its write after
    /// the handles were taken, or buffer events after the buffers were
    /// drained. So the flush repeats — await the writes, drain and write the
    /// buffers — until a round finds nothing, at most [`FLUSH_ROUNDS`] times.
    /// Past that (hooks arriving faster than the writes commit) the rest is
    /// left for the next flush; at shutdown it is lost with the process,
    /// like a `SIGKILL`'s interval.
    pub(crate) async fn flush(&self) {
        let mut hook_ids: Vec<String> = Vec::new();
        let mut usage_ids: Vec<String> = Vec::new();
        for _ in 0..FLUSH_ROUNDS {
            let writes =
                std::mem::take(&mut *self.writes.lock().unwrap_or_else(|e| e.into_inner()));
            let awaited = !writes.is_empty();
            for handle in writes {
                let _ = handle.await;
            }
            let drained: Vec<(String, Vec<HookEvent>)> = {
                let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
                buf.drain().collect()
            };
            let usage: Vec<(String, Vec<MemoryEvent>)> = {
                let mut buf = self.usage.lock().unwrap_or_else(|e| e.into_inner());
                buf.drain().collect()
            };
            if !awaited && drained.is_empty() && usage.is_empty() {
                break;
            }
            for (sid, events) in usage {
                write_usage_batch(&sid, &events).await;
                if !usage_ids.contains(&sid) {
                    usage_ids.push(sid);
                }
            }
            for (pid, events) in drained {
                write_batch(&pid, &events).await;
                if !hook_ids.contains(&pid) {
                    hook_ids.push(pid);
                }
            }
        }
        for pid in hook_ids {
            if self.prune_due(&pid) {
                let retention = self.retention_of(&pid);
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
        // Usage tables on their own throttle: a store's usage can be written
        // by a project whose hook events go elsewhere.
        for sid in usage_ids {
            let key = format!("usage:{sid}");
            if !self.prune_due(&key) {
                continue;
            }
            if let Some(days) = self.retention_of(&sid) {
                if let Err(e) = prune_memory_events(&sid, days).await {
                    tracing::warn!("memory_events prune for {sid}: {e}");
                }
            }
            if let Err(e) = reconcile_rollup(&sid).await {
                tracing::debug!("memory_usage reconcile for {sid}: {e}");
            }
            if let Err(e) = optimize_memory_events(&sid).await {
                tracing::debug!("memory_events optimize for {sid}: {e}");
            }
        }
    }

    fn retention_of(&self, id: &str) -> Option<u64> {
        self.retention
            .lock()
            .ok()
            .and_then(|r| r.get(id).copied())
            .flatten()
    }

    /// Whether `project_id`'s retention prune is due; marks it as run. Due on
    /// a project's first flush, then every [`HOOK_PRUNE_INTERVAL`].
    fn prune_due(&self, project_id: &str) -> bool {
        let mut last = self.last_prune.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match last.get(project_id) {
            Some(t) if now.duration_since(*t) < HOOK_PRUNE_INTERVAL => false,
            _ => {
                last.insert(project_id.to_string(), now);
                true
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

    #[cfg(test)]
    pub(crate) fn buffered_usage(&self, store_id: &str) -> usize {
        self.usage.lock().unwrap().get(store_id).map_or(0, Vec::len)
    }
}

pub(crate) async fn write_usage_batch(store_id: &str, events: &[MemoryEvent]) {
    if let Err(e) = ingest_memory_events(store_id, events, false).await {
        tracing::warn!(
            "memory_events: failed to write {} event(s) for store {store_id}: {e}",
            events.len()
        );
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

/// Whether the usage reconcile and compaction are due for `store_id` on the
/// daemon's maintenance path; marks them as run. Per store, process-wide
/// (one daemon per process), every [`HOOK_PRUNE_INTERVAL`].
fn usage_housekeeping_due(store_id: &str) -> bool {
    static LAST: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
    let mut last = LAST
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match last.get(store_id) {
        Some(t) if now.duration_since(*t) < HOOK_PRUNE_INTERVAL => false,
        _ => {
            last.retain(|_, t| now.duration_since(*t) < HOOK_PRUNE_INTERVAL);
            last.insert(store_id.to_string(), now);
            true
        }
    }
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
    // Usage JSONL written by in-process writers (CLI, MCP, hooks in their
    // own process), for the project and the shared stores it reads. The
    // daemon's own buffer prunes on its flush, so no retention here.
    crate::ops::usage::maintain_project_usage(
        &crate::ops::usage::StoreRef {
            id: project.project_id.clone(),
            root: project.root.clone(),
        },
        false,
        project.stats_enabled,
        None,
        &usage_housekeeping_due,
    )
    .await;
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
    fn usage_is_buffered_per_owning_store_and_gated_by_stats() {
        let t = HookTelemetry::default();
        let ev = |store: &str| {
            MemoryEvent::new(
                "m",
                store,
                crate::ops::UsageKind::Injected,
                crate::ops::Surface::HookPrompt,
            )
        };
        assert!(t.record_usage(&project(false), vec![ev("a")]).is_empty());
        assert_eq!(t.buffered_usage("a"), 0, "stats disabled records nothing");
        let p = project(true);
        assert!(t.record_usage(&p, vec![ev("a"), ev("b")]).is_empty());
        assert_eq!((t.buffered_usage("a"), t.buffered_usage("b")), (1, 1));
        let full = t.record_usage(&p, (0..HOOK_FLUSH_BATCH).map(|_| ev("a")).collect());
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].0, "a");
        assert_eq!(full[0].1.len(), HOOK_FLUSH_BATCH);
        assert_eq!(t.buffered_usage("a"), 1, "the remainder stays buffered");
    }

    /// A batch write spawned while a flush is running (a hook served during
    /// shutdown filling a batch) is awaited by that flush too.
    #[tokio::test]
    async fn flush_awaits_writes_spawned_while_it_runs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let t = Arc::new(HookTelemetry::default());
        let done = Arc::new(AtomicBool::new(false));
        let (t2, done2) = (Arc::clone(&t), Arc::clone(&done));
        // The first write, awaited by the flush, spawns a second one — as a
        // hook served mid-flush would.
        let first = tokio::spawn(async move {
            let late = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                done2.store(true, Ordering::SeqCst);
            });
            t2.writes.lock().unwrap().push(late);
        });
        t.writes.lock().unwrap().push(first);
        t.flush().await;
        assert!(
            done.load(Ordering::SeqCst),
            "the late batch must commit before flush returns"
        );
    }

    #[test]
    fn maintenance_is_throttled_per_project() {
        let t = HookTelemetry::default();
        assert!(t.maintenance_due(Path::new("/a")));
        assert!(!t.maintenance_due(Path::new("/a")));
        assert!(t.maintenance_due(Path::new("/b")));
    }

    /// The daemon's per-pass maintenance drains every time but reconciles
    /// and compacts a store's usage at most once per interval, however many
    /// projects reach that store (every project reaches the global one).
    #[test]
    fn usage_housekeeping_is_throttled_per_store() {
        assert!(usage_housekeeping_due("usage-throttle-a"));
        assert!(!usage_housekeeping_due("usage-throttle-a"));
        assert!(usage_housekeeping_due("usage-throttle-b"));
        assert!(!usage_housekeeping_due("usage-throttle-b"));
    }

    #[test]
    fn prune_is_due_on_a_projects_first_flush_then_throttled() {
        let t = HookTelemetry::default();
        assert!(t.prune_due("a"));
        assert!(!t.prune_due("a"));
        assert!(
            t.prune_due("b"),
            "one project's prune must not delay another's"
        );
    }

    #[test]
    fn env_mismatch_names_the_first_differing_key() {
        let mut client = std::collections::BTreeMap::new();
        let mut daemon = std::collections::BTreeMap::new();
        assert_eq!(env_mismatch(&client, &daemon), None);
        // Set on one side only counts as a difference.
        client.insert("ENGRAMDB_DATA_DIR".to_string(), "/a".to_string());
        assert_eq!(env_mismatch(&client, &daemon), Some("ENGRAMDB_DATA_DIR"));
        daemon.insert("ENGRAMDB_DATA_DIR".to_string(), "/a".to_string());
        assert_eq!(env_mismatch(&client, &daemon), None);
        // Variables outside the list are ignored.
        client.insert("EDITOR".to_string(), "vi".to_string());
        assert_eq!(env_mismatch(&client, &daemon), None);
    }
}
