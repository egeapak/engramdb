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

/// Internal state of the [`DaemonCell`].
struct State {
    /// The most recently verified-live daemon handle, or `None` if none is
    /// cached or if the cached one was found dead.
    current: Option<Arc<DaemonHandle>>,
    /// When the most recent *failed* spawn attempt occurred. Reset to `None`
    /// after a successful spawn so a confirmed-dead daemon can be respawned
    /// immediately — only failed spawns are rate-limited.
    last_spawn_attempt: Option<Instant>,
}

/// A re-resolvable cell holding an optional live [`DaemonHandle`].
///
/// Unlike a `OnceCell`, this re-validates the cached handle on every call and
/// can re-spawn a dead daemon. Spawn attempts are rate-limited to at most one
/// per `idle_timeout/3` window to prevent spawn storms, but only *failed*
/// spawns consume the window — a confirmed-successful spawn resets the timer
/// so the next death is recoverable immediately.
pub struct DaemonCell {
    state: Mutex<State>,
}

impl DaemonCell {
    /// Create a new empty cell (no daemon resolved yet).
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                current: None,
                last_spawn_attempt: None,
            }),
        }
    }

    /// Resolve a live daemon handle per `policy`.
    ///
    /// - Re-validates the cached handle each call (a cached handle whose daemon
    ///   died is dropped).
    /// - Rate-limits spawn attempts to one per `max(1s, idle_secs/3)` window,
    ///   but only for *failed* spawns — a confirmed successful spawn resets
    ///   `last_spawn_attempt` to `None` so a newly-dead daemon can be
    ///   respawned immediately.
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

        // ConnectOrSpawn, with backoff. Only failed spawns consume the window.
        let window = Duration::from_secs((idle_secs / 3).max(1));
        if let Some(t) = st.last_spawn_attempt {
            if t.elapsed() < window {
                return None;
            }
        }

        // Mark the attempt before we try, so a concurrent waiter (if the
        // lock were not held) would see it. The lock serialises this anyway,
        // but the stamp must precede the spawn, not follow it.
        st.last_spawn_attempt = Some(Instant::now());

        let h = DaemonHandle::connect_or_spawn(sock, idle_secs).await;

        if h.is_some() {
            // Confirmed successful spawn: reset the backoff timer so the next
            // death can be recovered from immediately without waiting a window.
            st.last_spawn_attempt = None;
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
