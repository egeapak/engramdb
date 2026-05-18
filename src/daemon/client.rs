//! Client handle: connect to the daemon, auto-spawning it if absent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::BufReader;
use tokio::net::UnixStream;

use super::protocol::{read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse};
use super::PROTOCOL_VERSION;

/// A connection factory for the shared daemon.
///
/// Each request opens a short-lived connection (connecting to a Unix socket is
/// sub-millisecond), which keeps the client free of reconnect/pool state — the
/// daemon, not the handle, is the long-lived thing.
pub struct DaemonHandle {
    socket: PathBuf,
}

impl DaemonHandle {
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

    /// True if a daemon answers `Ping` with a matching protocol version.
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
    pub async fn request(&self, req: DaemonRequest) -> anyhow::Result<DaemonResponse> {
        let stream = UnixStream::connect(&self.socket).await?;
        let (read_half, mut write_half) = stream.into_split();
        write_msg(&mut write_half, &req).await?;
        let mut reader = BufReader::new(read_half);
        match read_msg::<_, DaemonResponse>(&mut reader).await? {
            Some(resp) => Ok(resp),
            None => Err(anyhow::anyhow!(
                "daemon closed connection without a response"
            )),
        }
    }
}
