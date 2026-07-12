//! Shared daemon-or-in-process provider resolver.
//!
//! This module provides [`DaemonPolicy`], which expresses how a front-end may
//! obtain model providers, and [`DaemonCell`], the re-resolvable cell that
//! backs both the MCP server and CLI, replacing the permanent `OnceCell`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use super::DaemonHandle;
use crate::ops::{resolve_backend, resolve_engine_providers, EngineProviders, ProviderCache};
use crate::types::{EmbeddingBackend, EngramConfig};

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
}

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
            }),
        }
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
        if let Some(h) = DaemonHandle::connect_only(sock.clone()).await {
            st.current = Some(Arc::clone(&h));
            return Some(h);
        }

        if policy == DaemonPolicy::ConnectOnly {
            return None;
        }

        // ConnectOrSpawn, with backoff: failed spawns impose the long
        // `idle/3` window, successful ones the short crash-loop cooldown.
        if let Some((t, window)) = st.last_spawn {
            if t.elapsed() < window {
                return None;
            }
        }

        // Mark the attempt (pessimistically, with the failure window) before
        // we try, so a concurrent waiter (if the lock were not held) would
        // see it. The lock serialises this anyway, but the stamp must
        // precede the spawn, not follow it.
        let failure_window = Duration::from_secs((idle_secs / 3).max(1));
        st.last_spawn = Some((Instant::now(), failure_window));

        let h = DaemonHandle::connect_or_spawn(sock, idle_secs).await;

        if h.is_some() {
            // Confirmed successful spawn: shrink the backoff to the short
            // cooldown so a daemon that later dies is recovered quickly —
            // but never immediately, or a ponge-then-crash daemon would be
            // respawned on every single resolve (spawn storm).
            st.last_spawn = Some((Instant::now(), SPAWN_SUCCESS_COOLDOWN));
        }

        st.current = h.clone();
        h
    }
}

impl Default for DaemonCell {
    fn default() -> Self {
        Self::new()
    }
}

/// What to load when the daemon path is unavailable (disabled, unreachable,
/// or forbidden by policy).
pub enum InProcessFallback<'a> {
    /// Load a single-session bundle. Right for one-shot callers (the CLI):
    /// no concurrency, process exits after the call.
    Single,
    /// Serve pooled bundles from the given process-wide cache. Right for
    /// long-lived multi-session callers (the MCP server).
    Pool(&'a ProviderCache),
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
/// Graceful fallback is the contract: if the daemon is disabled in config,
/// the policy is `InProcess`, or the daemon is unreachable, this returns
/// in-process providers.
pub async fn resolve_providers_with(
    cell: &DaemonCell,
    config: &EngramConfig,
    backend: Option<EmbeddingBackend>,
    dir: &Path,
    policy: DaemonPolicy,
    fallback: InProcessFallback<'_>,
) -> EngineProviders {
    if config.daemon.enabled && policy != DaemonPolicy::InProcess {
        let idle = config.daemon.idle_timeout_secs;
        let socket = super::resolve_socket(None, &config.daemon);
        // The re-resolvable cell health-checks a cached handle and re-spawns
        // a dead daemon, so a session that outlived its daemon heals here
        // instead of degrading to in-process forever.
        if let Some(handle) = cell.get(&socket, idle, policy).await {
            // Send the resolved concrete backend so the daemon's provider
            // key matches ours regardless of the daemon's environment.
            let resolved_backend = Some(resolve_backend(config.embeddings.backend, backend));
            if let Some(providers) = super::remote_providers(
                handle,
                dir.to_string_lossy().into_owned(),
                resolved_backend,
                config,
            )
            .await
            {
                return providers;
            }
        }
    }
    match fallback {
        InProcessFallback::Single => resolve_engine_providers(config, backend, 1),
        InProcessFallback::Pool(cache) => cache.get(config, backend).await,
    }
}

/// [`resolve_providers_with`] with the one-shot [`InProcessFallback::Single`]
/// fallback — the CLI's default shape.
pub async fn resolve_providers(
    cell: &DaemonCell,
    config: &EngramConfig,
    backend: Option<EmbeddingBackend>,
    dir: &Path,
    policy: DaemonPolicy,
) -> EngineProviders {
    resolve_providers_with(
        cell,
        config,
        backend,
        dir,
        policy,
        InProcessFallback::Single,
    )
    .await
}
