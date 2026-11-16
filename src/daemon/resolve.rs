//! Shared daemon-or-in-process provider resolver.
//!
//! This module provides [`DaemonPolicy`], which expresses how a front-end may
//! obtain model providers, and [`DaemonCell`], the re-resolvable cell that
//! backs both the MCP server and CLI, replacing the permanent `OnceCell`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::client::{Health, SpawnOutcome};
use super::protocol::ModelSelection;
use super::DaemonHandle;
use crate::ops::{resolve_engine_providers, EngineProviders, ProviderCache};
use crate::types::{DaemonConfig, EmbeddingBackend, EngramConfig};

/// How a front-end may obtain model providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonPolicy {
    /// Use a live daemon, spawning one if absent (MCP default).
    ConnectOrSpawn,
    /// Use a live daemon only if already running, else in-process (CLI default).
    ConnectOnly,
    /// Never touch the daemon.
    InProcess,
}

/// Cooldown imposed after a spawn that *succeeded* (the daemon answered
/// Ping). Deliberately short — a healthy daemon that later dies should be
/// recoverable quickly — but non-zero: "success" only means the daemon
/// bound the socket and ponged, which requires no model load. A daemon that
/// ponges and then crashes on its first model load (bad ORT runtime, OOM)
/// would otherwise reset the backoff on every cycle and every tool call
/// would fork a fresh doomed daemon.
const SPAWN_SUCCESS_COOLDOWN: Duration = Duration::from_secs(10);

/// Internal state of the [`DaemonCell`].
struct State {
    /// The most recently verified-live daemon handle, or `None` if none is
    /// cached or if the cached one was found dead.
    current: Option<Arc<DaemonHandle>>,
    /// The most recent spawn attempt, paired with the backoff window it
    /// imposes: `idle/3` for a failed spawn, the short
    /// [`SPAWN_SUCCESS_COOLDOWN`] for a successful one.
    last_spawn: Option<(Instant, Duration)>,
    /// A newer daemon answered on the socket: until this instant `get`
    /// neither pings nor spawns. Pinging it would keep refreshing its idle
    /// clock (it never reaps while an old session pings), and each ping costs
    /// a round-trip per tool call for an answer that cannot change.
    incompatible_until: Option<Instant>,
}

/// How long a cell leaves a newer daemon alone before probing it again: the
/// daemon's idle timeout, but at least this.
const INCOMPATIBLE_MIN_WINDOW: Duration = Duration::from_secs(600);

/// A re-resolvable cell holding an optional live [`DaemonHandle`].
///
/// Unlike a `OnceCell`, this re-validates the cached handle on every call and
/// can re-spawn a dead daemon. Spawn attempts are rate-limited: a failed
/// spawn imposes an `idle_timeout/3` backoff window, a successful one only
/// the short [`SPAWN_SUCCESS_COOLDOWN`] — so a daemon death is recovered
/// quickly without letting a ponge-then-crash daemon trigger a spawn storm.
pub struct DaemonCell {
    state: Mutex<State>,
}

impl DaemonCell {
    /// Create a new empty cell (no daemon resolved yet).
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                current: None,
                last_spawn: None,
                incompatible_until: None,
            }),
        }
    }

    /// The backoff window the last spawn attempt imposed (test inspection).
    #[cfg(test)]
    pub(crate) async fn spawn_backoff(&self) -> Option<Duration> {
        self.state.lock().await.last_spawn.map(|(_, window)| window)
    }

    /// Resolve a live daemon handle per `policy`.
    ///
    /// - Re-validates the cached handle each call (a cached handle whose daemon
    ///   died is dropped).
    /// - Rate-limits spawn attempts: one per `max(1s, idle_secs/3)` window
    ///   after a *failed* spawn, one per [`SPAWN_SUCCESS_COOLDOWN`] after a
    ///   successful one — quick recovery from a daemon death without a spawn
    ///   storm when the daemon crash-loops right after binding.
    pub async fn get(
        &self,
        socket: &Path,
        idle_secs: u64,
        policy: DaemonPolicy,
    ) -> Option<Arc<DaemonHandle>> {
        if policy == DaemonPolicy::InProcess {
            return None;
        }

        let mut st = self.state.lock().await;

        // A newer daemon holds the socket: leave it alone (no ping, no
        // spawn) until the window passes. After it, one probe is allowed —
        // the newer daemon may have been reaped and the socket freed.
        if let Some(until) = st.incompatible_until {
            if Instant::now() < until {
                return None;
            }
            st.incompatible_until = None;
        }
        let incompatible_window = Duration::from_secs(idle_secs).max(INCOMPATIBLE_MIN_WINDOW);

        // Fast path: cached handle still answers Ping.
        if let Some(h) = &st.current {
            if h.check_health().await {
                return Some(Arc::clone(h));
            }
            st.current = None; // dead — drop it
        }

        // Try a bare connect (no spawn) first — covers the case where the
        // daemon is alive but wasn't cached (fresh process, or after a
        // previous poll loop that cleared `current`).
        let sock = socket.to_path_buf();
        match DaemonHandle::probe(sock.clone()).await {
            (Health::Healthy, h) => {
                st.current = Some(Arc::clone(&h));
                return Some(h);
            }
            (Health::Incompatible, _) => {
                st.incompatible_until = Some(Instant::now() + incompatible_window);
                return None;
            }
            // Absent, or an older daemon that was just asked to shut down:
            // a spawn (below) replaces it.
            (Health::Absent | Health::Evicting, _) => {}
        }

        if policy == DaemonPolicy::ConnectOnly {
            tracing::debug!(
                "no daemon listening on {} and policy is ConnectOnly; using in-process models",
                socket.display()
            );
            return None;
        }

        // ConnectOrSpawn, with backoff: failed spawns impose the long
        // `idle/3` window, successful ones the short crash-loop cooldown.
        if let Some((t, window)) = st.last_spawn {
            if t.elapsed() < window {
                // Warn, not debug: the caller asked for a daemon and is being
                // refused one. Without this the backoff is indistinguishable
                // from "the daemon simply isn't wanted", and a user watching a
                // crash-looping daemon sees no explanation for why nothing is
                // even being attempted.
                tracing::warn!(
                    "not spawning a daemon: a previous attempt {:.0}s ago is still within its \
                     {:.0}s backoff; using in-process models (see the daemon log for why the \
                     last attempt failed)",
                    t.elapsed().as_secs_f64(),
                    window.as_secs_f64()
                );
                return None;
            }
        }

        // Mark the attempt (pessimistically, with the failure window) before
        // we try, so a concurrent waiter (if the lock were not held) would
        // see it. The lock serialises this anyway, but the stamp must
        // precede the spawn, not follow it.
        let failure_window = Duration::from_secs((idle_secs / 3).max(1));
        st.last_spawn = Some((Instant::now(), failure_window));

        // No idle timeout on the command line: the daemon reads the global
        // config itself (`idle_secs` above only sizes this cell's backoff).
        let h = match DaemonHandle::connect_or_spawn_outcome(sock, None).await {
            SpawnOutcome::Connected(h) => {
                // Confirmed successful spawn: shrink the backoff to the short
                // cooldown so a daemon that later dies is recovered quickly —
                // but never immediately, or a ponge-then-crash daemon would be
                // respawned on every single resolve (spawn storm).
                st.last_spawn = Some((Instant::now(), SPAWN_SUCCESS_COOLDOWN));
                Some(h)
            }
            SpawnOutcome::StillEvicting => {
                // Nothing failed: the older daemon is draining. The next
                // resolve a few seconds later finds the socket free, so use
                // the short cooldown, not the failure window.
                st.last_spawn = Some((Instant::now(), SPAWN_SUCCESS_COOLDOWN));
                None
            }
            SpawnOutcome::Incompatible => {
                st.incompatible_until = Some(Instant::now() + incompatible_window);
                None
            }
            SpawnOutcome::SpawnFailed => None,
        };

        st.current = h.clone();
        h
    }
}

impl Default for DaemonCell {
    fn default() -> Self {
        Self::new()
    }
}

/// Unload the in-process fallback models of a session the daemon now serves.
///
/// A transient daemon failure makes a long-lived MCP session load its models
/// in-process (`InProcessFallback::Pool`), and that cache has no sweeper of
/// its own, so those models stayed resident for the session's lifetime even
/// after the daemon came back. The MCP heartbeat calls this on each tick:
/// when the daemon answered (`daemon_healthy`), demands older than `idle`
/// expire and unused models unload. In-process demands are stamped only by
/// in-process requests, so once the daemon serves the session they age out
/// after one idle window; a model a request still holds is kept. When the
/// daemon is unreachable nothing is swept, so in-process-as-primary keeps
/// its models. Returns how many models were unloaded.
pub async fn sweep_in_process_models(
    cache: &ProviderCache,
    daemon_healthy: bool,
    idle: Duration,
) -> usize {
    if !daemon_healthy {
        return 0;
    }
    cache.evict_idle(idle).await
}

/// What to load when the daemon path is unavailable (disabled, unreachable,
/// or forbidden by policy).
pub enum InProcessFallback<'a> {
    /// Load single-session models. Right for one-shot callers (the CLI):
    /// no concurrency, process exits after the call.
    Single,
    /// Serve pooled models from the given process-wide cache. Right for
    /// long-lived multi-session callers (the MCP server).
    Pool(&'a ProviderCache),
    /// Serve models from the given cache at a demand of one session. Right for a
    /// sequential batch over several projects (`projects discover` /
    /// `repair`): the model loads once for the batch, and no extra sessions
    /// are built that a sequential caller could never use. The one-session
    /// size applies to the in-process load only — the daemon still gets the
    /// project's own `pool_size`, so it serves these calls from the same
    /// shared models as every other session instead of building more.
    SequentialPool(&'a ProviderCache),
}

/// Resolve model-backed providers for a retrieval engine, routing through the
/// daemon when available and `policy` permits, or loading in-process per
/// `fallback`.
///
/// This is the **single shared resolver** for both the MCP server and the
/// CLI; the front-ends differ only in the `policy` and `fallback` they pass:
/// - MCP uses `ConnectOrSpawn` + `Pool` (auto-spawns the daemon when absent;
///   pooled in-process providers when it can't).
/// - CLI uses `ConnectOnly` + `Single` by default (uses a live daemon, else a
///   one-shot in-process load).
/// - Either front-end can be overridden to `InProcess` to skip the daemon.
///
/// `config` is the caller's **project** config and decides only *which models*
/// to load (sent to the daemon as a [`ModelSelection`]). Everything about the
/// daemon itself — `enabled`, the socket, the idle timeout — comes from the
/// **global** config, loaded here so no caller can pass a project's `[daemon]`
/// by mistake.
///
/// Graceful fallback is the contract: if the daemon is disabled in config,
/// the policy is `InProcess`, or the daemon is unreachable, this returns
/// in-process providers.
pub async fn resolve_providers_with(
    cell: &DaemonCell,
    config: &EngramConfig,
    backend: Option<EmbeddingBackend>,
    policy: DaemonPolicy,
    fallback: InProcessFallback<'_>,
) -> EngineProviders {
    if policy != DaemonPolicy::InProcess {
        let daemon = crate::storage::config::load_global_config_or_default()
            .await
            .daemon;
        if let Some(providers) = remote_for(cell, &daemon, config, backend, policy).await {
            return providers;
        }
    }
    match fallback {
        InProcessFallback::Single => resolve_engine_providers(config, backend, 1),
        InProcessFallback::Pool(cache) => cache.get(config, backend).await,
        InProcessFallback::SequentialPool(cache) => {
            let mut single = config.clone();
            single.embeddings.pool_size = Some(1);
            cache.get(&single, backend).await
        }
    }
}

/// The daemon half of [`resolve_providers_with`], with the global `daemon`
/// settings passed in. `None` means "use in-process providers".
async fn remote_for(
    cell: &DaemonCell,
    daemon: &DaemonConfig,
    config: &EngramConfig,
    backend: Option<EmbeddingBackend>,
    policy: DaemonPolicy,
) -> Option<EngineProviders> {
    if !daemon.enabled {
        return None;
    }
    let socket = super::resolve_socket(None, daemon);
    // The re-resolvable cell health-checks a cached handle and re-spawns a
    // dead daemon, so a session that outlived its daemon heals here instead
    // of degrading to in-process forever.
    let handle = cell.get(&socket, daemon.idle_timeout_secs, policy).await?;
    // Send the resolved concrete backend so the daemon's provider key matches
    // ours regardless of the daemon's environment.
    super::remote_providers(handle, ModelSelection::from_config(config, backend), config).await
}

/// Providers from an **already running** daemon, or `None`: never spawns one
/// and never loads a model in-process.
///
/// For latency-bound callers that can do without models — the hooks, which
/// fall back to keyword heuristics. A one-shot in-process load (~240 ms of
/// ONNX init per call) is exactly the cost those callers must not pay, so
/// unlike [`resolve_providers_with`] there is no fallback. Honors the global
/// `[daemon].enabled` like every other resolve.
pub async fn resolve_daemon_providers(
    cell: &DaemonCell,
    config: &EngramConfig,
    backend: Option<EmbeddingBackend>,
) -> Option<EngineProviders> {
    let daemon = crate::storage::config::load_global_config_or_default()
        .await
        .daemon;
    remote_for(cell, &daemon, config, backend, DaemonPolicy::ConnectOnly).await
}

/// [`resolve_providers_with`] with the one-shot [`InProcessFallback::Single`]
/// fallback — the CLI's default shape.
pub async fn resolve_providers(
    cell: &DaemonCell,
    config: &EngramConfig,
    backend: Option<EmbeddingBackend>,
    policy: DaemonPolicy,
) -> EngineProviders {
    resolve_providers_with(cell, config, backend, policy, InProcessFallback::Single).await
}
