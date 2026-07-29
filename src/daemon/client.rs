//! Client handle: connect to the daemon, auto-spawning it if absent.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::BufReader;

use super::protocol::{read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse, DaemonStatus};
use super::PROTOCOL_VERSION;

/// One-shot request over a fresh connection to a socket, without spawning.
/// Used by the `engramdb daemon` CLI subcommands and `doctor`/`stats`, which
/// only ever talk to an already-running daemon (never auto-spawn).
async fn oneshot(socket: &Path, op: DaemonOp) -> anyhow::Result<DaemonResponse> {
    let fut = async {
        let stream = super::transport::connect(socket).await?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        write_msg(
            &mut write_half,
            &DaemonRequest {
                dir: String::new(),
                backend: None,
                op,
            },
        )
        .await?;
        let mut reader = BufReader::new(read_half);
        match read_msg::<_, DaemonResponse>(&mut reader).await? {
            Some(resp) => Ok(resp),
            None => Err(anyhow::anyhow!(
                "daemon closed connection without a response"
            )),
        }
    };
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("daemon request timed out")))
}

/// Query a running daemon's status. `Ok(None)` means no daemon is listening
/// on `socket` (not an error — the daemon is auto-spawned on demand).
pub async fn query_status(socket: &Path) -> anyhow::Result<Option<DaemonStatus>> {
    match oneshot(socket, DaemonOp::Status).await {
        Ok(DaemonResponse::Status(s)) => Ok(Some(s)),
        Ok(other) => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        // Connection refused / no socket ⇒ not running.
        Err(_) => Ok(None),
    }
}

/// Ask a running daemon to exit. `Ok(false)` means none was running.
pub async fn request_shutdown(socket: &Path) -> anyhow::Result<bool> {
    match oneshot(socket, DaemonOp::Shutdown).await {
        Ok(DaemonResponse::ShuttingDown) => Ok(true),
        Ok(other) => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        Err(_) => Ok(false),
    }
}

/// Whether `daemon` is a strictly older protocol version than `client`.
/// Versions are small integers serialized as strings; anything unparseable is
/// treated as NOT older (likely a future format — never kill what we can't
/// compare).
fn version_is_older(daemon: &str, client: &str) -> bool {
    match (daemon.trim().parse::<u64>(), client.trim().parse::<u64>()) {
        (Ok(d), Ok(c)) => d < c,
        _ => false,
    }
}

/// Whether a daemon's crate version is older than this build's.
///
/// `None` means a pre-protocol-4 daemon, which is by definition older. Unlike
/// the protocol version this is semver, so compare numerically per component —
/// a lexical compare would rank `0.10.0` below `0.9.0`. An unparseable version
/// is treated as not-older, so a daemon from an unexpected build is left alone
/// rather than killed in a loop.
fn build_is_older(daemon: Option<&str>, client: &str) -> bool {
    let Some(daemon) = daemon else {
        return true;
    };
    let parts = |v: &str| -> Option<(u64, u64, u64)> {
        let mut it = v.trim().split('.').map(str::parse::<u64>);
        let triple = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
        Some(triple)
    };
    match (parts(daemon), parts(client)) {
        (Some(d), Some(c)) => d < c,
        _ => false,
    }
}

/// A connection factory for the shared daemon.
///
/// Each request opens a short-lived connection (connecting to a Unix socket is
/// sub-millisecond), which keeps the client free of reconnect/pool state — the
/// daemon, not the handle, is the long-lived thing.
pub struct DaemonHandle {
    socket: PathBuf,
}

impl DaemonHandle {
    /// Upper bound on a single request/response round-trip. Generous so a
    /// cold first call (which triggers the daemon's ~240ms+ model load, plus
    /// inference over a memory's chunks) never trips it; tight enough that a
    /// wedged daemon doesn't hang a tool call indefinitely.
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

    /// Upper bound on a `Ping` health check. Ping requires no model load —
    /// the daemon answers it as soon as it binds the socket — so a healthy
    /// daemon responds in microseconds. Kept far below [`Self::REQUEST_TIMEOUT`]
    /// because health checks run while the `DaemonCell` mutex is held: a
    /// wedged-but-accepting daemon must stall a tool call (and the heartbeat)
    /// for at most a couple of seconds before falling back, not a minute.
    const PING_TIMEOUT: Duration = Duration::from_secs(2);

    /// Get a handle to a live daemon, spawning one if none is reachable.
    ///
    /// Returns `None` if no daemon could be reached or started, or if a
    /// reachable daemon speaks a different protocol version — callers then
    /// fall back to in-process model loading. Auto-spawn is race-safe: only
    /// one process can bind the socket, so of several concurrently-spawned
    /// daemons one survives and the rest exit; every client converges on the
    /// survivor.
    pub async fn connect_or_spawn(socket: PathBuf, idle_timeout_secs: u64) -> Option<Arc<Self>> {
        let handle = Self {
            socket: socket.clone(),
        };
        if handle.healthy().await {
            return Some(Arc::new(handle));
        }

        Self::spawn_daemon(&socket, idle_timeout_secs);

        // The daemon must load nothing to answer Ping, so it becomes
        // reachable as soon as it binds the socket. Bounded retry (~3.8s
        // total) so a failed spawn degrades to in-process instead of hanging.
        for delay_ms in [25u64, 50, 75, 100, 150, 200, 300, 400, 500, 750, 1000, 1250] {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            if handle.healthy().await {
                return Some(Arc::new(handle));
            }
        }
        tracing::warn!("engramdb daemon unreachable after spawn; using in-process models");
        None
    }

    /// Connect to an already-running daemon without spawning. Returns `None`
    /// if no daemon is listening on `socket` or if it fails the protocol
    /// version check. Used by [`DaemonCell`] to probe liveness before
    /// deciding whether to spawn.
    pub(crate) async fn connect_only(socket: PathBuf) -> Option<Arc<Self>> {
        let handle = Self { socket };
        if handle.healthy().await {
            Some(Arc::new(handle))
        } else {
            None
        }
    }

    /// True if a daemon answers `Ping` with a matching protocol version.
    pub(crate) async fn check_health(&self) -> bool {
        self.healthy().await
    }

    async fn healthy(&self) -> bool {
        match self
            .request_with_timeout(
                DaemonRequest {
                    dir: String::new(),
                    backend: None,
                    op: DaemonOp::Ping,
                },
                Self::PING_TIMEOUT,
            )
            .await
        {
            Ok(DaemonResponse::Pong { version, build }) if version == PROTOCOL_VERSION => {
                // Same wire, older binary: its cached provider bundles are from
                // the previous release, so it can serve vectors from a model
                // this build no longer uses — silently, because the manifest
                // fingerprint is computed client-side. Evict it the same way an
                // older protocol is evicted. Newer build: leave it alone, for
                // the same reason as the protocol case below.
                match build.as_deref() {
                    Some(env!("CARGO_PKG_VERSION")) => true,
                    b => {
                        let reported = b.unwrap_or("pre-4");
                        tracing::warn!(
                            "engramdb daemon build {reported} differs from client {}; requesting shutdown so a current daemon can start",
                            env!("CARGO_PKG_VERSION")
                        );
                        if build_is_older(b, env!("CARGO_PKG_VERSION")) {
                            let _ = oneshot(&self.socket, DaemonOp::Shutdown).await;
                        }
                        false
                    }
                }
            }
            Ok(DaemonResponse::Pong { version, .. }) => {
                // A stale daemon would otherwise live forever: every health
                // check that rejects it is itself a served request that
                // refreshes its idle clock, and the socket stays bound so a
                // replacement can never start ("falls back until the old
                // daemon reaps" never happens — the pings prevent reaping).
                // When the daemon is provably OLDER than this client, ask it
                // to shut down so an up-to-date one can be spawned. The
                // reverse direction (daemon newer than this client) must NOT
                // kill it — an old CLI would repeatedly assassinate the
                // daemon that newer sessions keep respawning.
                if version_is_older(&version, PROTOCOL_VERSION) {
                    tracing::warn!(
                        "engramdb daemon protocol {version} is older than client {PROTOCOL_VERSION}; requesting shutdown so a current daemon can start"
                    );
                    let _ = oneshot(&self.socket, DaemonOp::Shutdown).await;
                } else {
                    tracing::warn!(
                        "engramdb daemon protocol mismatch (daemon {version}, client {PROTOCOL_VERSION}); using in-process models"
                    );
                }
                false
            }
            _ => false,
        }
    }

    /// Spawn a detached daemon. Best-effort and non-blocking: the spawn is
    /// fire-and-forget (the daemon self-terminates on idle-timeout and is
    /// reparented to init if this process exits first). Failures are logged;
    /// the retry loop in [`Self::connect_or_spawn`] surfaces them as the
    /// in-process fallback.
    ///
    /// The child **is** reaped: a background task awaits `wait()` so that when
    /// the daemon exits (idle-timeout, shutdown, losing the socket-bind race)
    /// while a long-lived parent (`serve`) is still running, it does not
    /// linger as a zombie. `kill_on_drop` stays false (tokio's default), so
    /// the daemon outlives the parent exactly as before.
    fn spawn_daemon(socket: &std::path::Path, idle_timeout_secs: u64) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("cannot locate current exe to spawn daemon: {e}");
                return;
            }
        };
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("daemon")
            .arg("run")
            .arg("--socket")
            .arg(socket)
            .arg("--idle-timeout")
            .arg(idle_timeout_secs.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null());

        // This child is detached and outlives us, so nobody is watching its
        // stderr — it used to go to `/dev/null`, which meant every reason a
        // daemon could fail to start (bind error, missing ONNX runtime, panic
        // on first model load) was discarded. Callers still fall back to
        // in-process models, but now the reason is recoverable. If the log
        // can't be opened, fall back to discarding rather than inheriting:
        // inheriting would interleave daemon output into an MCP server's
        // stdio, which is a protocol stream.
        match super::logging::daemon_log_for_spawn() {
            Ok(file) => {
                cmd.stderr(std::process::Stdio::from(file));
            }
            Err(e) => {
                tracing::warn!(
                    "cannot open the daemon log ({e}); daemon diagnostics will be discarded"
                );
                cmd.stderr(std::process::Stdio::null());
            }
        }
        match cmd.spawn() {
            Ok(mut child) => {
                tracing::debug!("spawned engramdb daemon");
                // Reap the child when it exits so a long-lived parent process
                // (the MCP `serve` loop) doesn't accumulate zombies across
                // daemon idle-timeout/restart cycles. Purely an await on
                // process exit — it neither kills nor keeps the child alive.
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
            }
            Err(e) => tracing::warn!("failed to spawn engramdb daemon: {e}"),
        }
    }

    /// Wrap a known socket without probing or spawning. Test-only: production
    /// code must go through [`Self::connect_or_spawn`] so liveness and
    /// protocol version are verified.
    #[cfg(test)]
    pub(crate) fn connect_existing(socket: PathBuf) -> Arc<Self> {
        Arc::new(Self { socket })
    }

    /// Send one request and read its response over a fresh connection.
    ///
    /// Bounded by [`Self::REQUEST_TIMEOUT`]: a daemon that accepts the
    /// connection but then wedges (deadlocked model mutex, stuck ONNX thread)
    /// must not hang the agent's tool call forever — on timeout this errors so
    /// the caller can fall back to in-process models. The bound is generous
    /// enough for a cold first request that triggers the daemon's model load.
    pub async fn request(&self, req: DaemonRequest) -> anyhow::Result<DaemonResponse> {
        self.request_with_timeout(req, Self::REQUEST_TIMEOUT).await
    }

    /// [`Self::request`] with an explicit round-trip bound. Inference requests
    /// use [`Self::REQUEST_TIMEOUT`]; `Ping` health checks use the much
    /// shorter [`Self::PING_TIMEOUT`].
    async fn request_with_timeout(
        &self,
        req: DaemonRequest,
        timeout: Duration,
    ) -> anyhow::Result<DaemonResponse> {
        tokio::time::timeout(timeout, async {
            let stream = super::transport::connect(&self.socket).await?;
            let (read_half, mut write_half) = tokio::io::split(stream);
            write_msg(&mut write_half, &req).await?;
            let mut reader = BufReader::new(read_half);
            match read_msg::<_, DaemonResponse>(&mut reader).await? {
                Some(resp) => Ok(resp),
                None => Err(anyhow::anyhow!(
                    "daemon closed connection without a response"
                )),
            }
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("daemon request timed out")))
    }
}

#[cfg(test)]
mod version_tests {
    use super::{build_is_older, version_is_older};

    #[test]
    fn protocol_version_ordering() {
        assert!(version_is_older("3", "4"));
        assert!(!version_is_older("4", "4"));
        assert!(!version_is_older("5", "4"));
        // Unparseable: never evict something we cannot compare.
        assert!(!version_is_older("999.bogus", "4"));
    }

    #[test]
    fn build_version_ordering() {
        assert!(build_is_older(Some("0.8.0"), "0.9.0"));
        assert!(!build_is_older(Some("0.9.0"), "0.9.0"));
        assert!(!build_is_older(Some("0.10.0"), "0.9.0"));
        // A pre-protocol-4 daemon sends no build and is older by definition.
        assert!(build_is_older(None, "0.9.0"));
        assert!(!build_is_older(Some("not.a.version"), "0.9.0"));
    }

    /// Lexical comparison would rank `0.10.0` below `0.9.0` and make an
    /// upgraded client kill the newer daemon that other sessions respawn.
    #[test]
    fn build_comparison_is_numeric_not_lexical() {
        assert!(build_is_older(Some("0.9.0"), "0.10.0"));
        assert!(!build_is_older(Some("0.10.0"), "0.9.0"));
    }
}
