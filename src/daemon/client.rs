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
            .request(DaemonRequest {
                dir: String::new(),
                backend: None,
                op: DaemonOp::Ping,
            })
            .await
        {
            Ok(DaemonResponse::Pong { version }) if version == PROTOCOL_VERSION => true,
            Ok(DaemonResponse::Pong { version }) => {
                tracing::warn!(
                    "engramdb daemon protocol mismatch (daemon {version}, client {PROTOCOL_VERSION}); using in-process models"
                );
                false
            }
            _ => false,
        }
    }

    /// Spawn a detached daemon. Best-effort and non-blocking: the child is
    /// not awaited (it self-terminates on idle-timeout and is reparented to
    /// init if this process exits first). Failures are logged; the retry loop
    /// in [`Self::connect_or_spawn`] surfaces them as the in-process fallback.
    fn spawn_daemon(socket: &std::path::Path, idle_timeout_secs: u64) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("cannot locate current exe to spawn daemon: {e}");
                return;
            }
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("daemon")
            .arg("run")
            .arg("--socket")
            .arg(socket)
            .arg("--idle-timeout")
            .arg(idle_timeout_secs.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(_child) => tracing::debug!("spawned engramdb daemon"),
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
        tokio::time::timeout(Self::REQUEST_TIMEOUT, async {
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
