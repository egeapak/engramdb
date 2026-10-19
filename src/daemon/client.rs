//! Client handle: connect to the daemon, auto-spawning it if absent.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::BufReader;

use super::protocol::{
    read_msg, write_msg, DaemonOp, DaemonRequest, DaemonResponse, DaemonStatus, ModelSelection,
};
use super::PROTOCOL_VERSION;

/// One-shot request over a fresh connection to a socket, without spawning.
/// Used by the `engramdb daemon` CLI subcommands and `doctor`/`stats`, which
/// only ever talk to an already-running daemon (never auto-spawn).
async fn oneshot(socket: &Path, op: DaemonOp) -> anyhow::Result<DaemonResponse> {
    let fut = async {
        let stream = super::transport::connect(socket).await?;
        let (read_half, mut write_half) = tokio::io::split(stream);
        write_msg(&mut write_half, &DaemonRequest { models: None, op }).await?;
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

/// Outcome of offering a hook to a running daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDelegation {
    /// The daemon ran the hook. `stdout` is what the hook prints.
    Served { stdout: Option<String>, ok: bool },
    /// The daemon never ran it: none listening, an older protocol, no hook
    /// runner, or the request could not be written. Run it in-process.
    NotSent(String),
    /// The request was sent but no reply came back (timeout, connection
    /// dropped). The daemon may have run the hook, so running it again
    /// in-process could repeat its side effects; print nothing.
    Lost(String),
}

/// Upper bound on a hook round trip. Claude Code's own hook timeout is 60 s
/// by default, so a reply later than this has no one left to read it.
const HOOK_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound on the version check that precedes a hook request.
const HOOK_PING_TIMEOUT: Duration = Duration::from_secs(2);

/// Offer a hook to an already-running daemon. Never spawns one.
///
/// One connection carries both requests: a `Ping` first, so the hook is
/// only sent to a daemon that speaks this protocol (a v5 daemon cannot parse
/// the `Hook` op, and would drop the connection after reading it — which
/// would look like [`HookDelegation::Lost`]), then the `Hook` itself.
pub async fn delegate_hook(
    socket: &Path,
    call: &super::protocol::HookCall,
    dir: &Path,
    stdin: &str,
) -> HookDelegation {
    let stream = match super::transport::connect(socket).await {
        Ok(s) => s,
        Err(e) => return HookDelegation::NotSent(format!("connect: {e}")),
    };
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    let ping = async {
        write_msg(
            &mut write_half,
            &DaemonRequest {
                models: None,
                op: DaemonOp::Ping,
            },
        )
        .await?;
        read_msg::<_, DaemonResponse>(&mut reader).await
    };
    match tokio::time::timeout(HOOK_PING_TIMEOUT, ping).await {
        // An older build of the same protocol would run its own, older hook
        // handlers. Nothing on the hook path evicts it (it never spawns), so
        // run in-process until something else replaces that daemon.
        Ok(Ok(Some(DaemonResponse::Pong { version, build }))) if version == PROTOCOL_VERSION => {
            if build_is_older(build.as_deref(), env!("CARGO_PKG_VERSION")) {
                return HookDelegation::NotSent(format!(
                    "daemon build {} is older than {}",
                    build.as_deref().unwrap_or("unknown"),
                    env!("CARGO_PKG_VERSION")
                ));
            }
        }
        Ok(Ok(Some(DaemonResponse::Pong { version, .. }))) => {
            return HookDelegation::NotSent(format!("daemon speaks protocol {version}"))
        }
        Ok(Ok(other)) => {
            return HookDelegation::NotSent(format!("unexpected ping reply: {other:?}"))
        }
        Ok(Err(e)) => return HookDelegation::NotSent(format!("ping: {e}")),
        Err(_) => return HookDelegation::NotSent("ping timed out".to_string()),
    }

    let request = DaemonRequest {
        models: None,
        op: DaemonOp::Hook {
            call: call.clone(),
            dir: dir.to_path_buf(),
            stdin: stdin.to_string(),
            env: super::hooks::hook_env(),
        },
    };
    // The daemon only executes a complete frame, so a failed write means the
    // hook did not run.
    if let Err(e) = write_msg(&mut write_half, &request).await {
        return HookDelegation::NotSent(format!("write: {e}"));
    }
    match tokio::time::timeout(HOOK_TIMEOUT, read_msg::<_, DaemonResponse>(&mut reader)).await {
        Ok(Ok(Some(DaemonResponse::HookOutput { stdout, ok }))) => {
            HookDelegation::Served { stdout, ok }
        }
        // Refused before running anything (a daemon without a hook runner).
        Ok(Ok(Some(DaemonResponse::Error { message }))) => HookDelegation::NotSent(message),
        Ok(Ok(Some(other))) => HookDelegation::Lost(format!("unexpected reply: {other:?}")),
        Ok(Ok(None)) => HookDelegation::Lost("daemon closed the connection".to_string()),
        Ok(Err(e)) => HookDelegation::Lost(format!("read: {e}")),
        Err(_) => HookDelegation::Lost("timed out".to_string()),
    }
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
    ///
    /// The spawned daemon reads its idle timeout from the global config
    /// itself. `idle_override` exists only for `engramdb daemon restart
    /// --idle-timeout` (an explicit, user-typed value); every automatic spawn
    /// passes `None`, so no project's value can decide the shared daemon's
    /// lifetime.
    pub async fn connect_or_spawn(
        socket: PathBuf,
        idle_override: Option<u64>,
    ) -> Option<Arc<Self>> {
        let handle = Self {
            socket: socket.clone(),
        };
        if handle.healthy().await {
            return Some(Arc::new(handle));
        }

        Self::spawn_daemon(&socket, idle_override);

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

    /// Renew this session's lease on the daemon's models for `models`.
    ///
    /// A `Ping` that carries a model selection: the daemon marks each model
    /// the selection uses as in use, at its pool size (it never loads one for
    /// a `Ping`), so a session that is open but quiet keeps its models
    /// resident, and a model is unloaded once every session using it has
    /// ended. An older daemon ignores the selection and treats this as a
    /// plain `Ping`, so its models are never unloaded. Returns whether the
    /// daemon answered.
    pub async fn renew_lease(&self, models: ModelSelection) -> bool {
        matches!(
            self.request_with_timeout(
                DaemonRequest {
                    models: Some(models),
                    op: DaemonOp::Ping,
                },
                Self::PING_TIMEOUT,
            )
            .await,
            Ok(DaemonResponse::Pong { .. })
        )
    }

    /// True if a daemon answers `Ping` with a matching protocol version.
    pub(crate) async fn check_health(&self) -> bool {
        self.healthy().await
    }

    async fn healthy(&self) -> bool {
        match self
            .request_with_timeout(
                DaemonRequest {
                    models: None,
                    op: DaemonOp::Ping,
                },
                Self::PING_TIMEOUT,
            )
            .await
        {
            Ok(DaemonResponse::Pong { version, build }) if version == PROTOCOL_VERSION => {
                // Same wire, older binary: its cached models are from
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
    fn spawn_daemon(socket: &std::path::Path, idle_override: Option<u64>) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("cannot locate current exe to spawn daemon: {e}");
                return;
            }
        };
        let mut cmd = spawn_command(&exe, socket, idle_override);

        // This child is detached and outlives us, so nobody is watching its
        // stderr — it used to go to `/dev/null`, which meant every reason a
        // daemon could fail to start (bind error, missing ONNX runtime, panic
        // on first model load) was discarded. Callers still fall back to
        // in-process models, but now the reason is recoverable. If the log
        // can't be opened, fall back to discarding rather than inheriting:
        // inheriting would interleave daemon output into an MCP server's
        // stdio, which is a protocol stream.
        match super::logging::daemon_log_for_spawn(socket) {
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

/// The command line for a spawned daemon: `daemon run --socket <socket>`,
/// plus `--idle-timeout` only when `idle_override` is set.
///
/// The socket is always passed, because the client has already resolved it
/// and the daemon must bind exactly that path. The idle timeout is not: the
/// daemon reads `idle_timeout_secs` from the global config itself, which is
/// the same value every session's heartbeat interval is derived from.
fn spawn_command(exe: &Path, socket: &Path, idle_override: Option<u64>) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("daemon").arg("run").arg("--socket").arg(socket);
    if let Some(secs) = idle_override {
        cmd.arg("--idle-timeout").arg(secs.to_string());
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    // The daemon outlives the session that spawned it and serves every other
    // session, so it must not carry that session's environment (its
    // `GIT_DIR`, secrets, tool settings). It starts from an empty environment
    // plus the allowlist below.
    cmd.env_clear().envs(daemon_env(std::env::vars_os()));
    cmd
}

/// Variables a spawned daemon keeps, compared without regard to case
/// (Windows names are case-insensitive, and proxy variables come in both
/// cases on Unix).
const DAEMON_ENV_KEEP: &[&str] = &[
    // Identity, locale, temp dirs, and `PATH` (hooks run `git`).
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "LANG",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    // Where the ONNX runtime is found.
    "ORT_DYLIB_PATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    // Logging into `daemon.log`.
    "RUST_LOG",
    "RUST_BACKTRACE",
    // Where SessionEnd finds the transcript it copies.
    "CLAUDE_CONFIG_DIR",
    // Model backends and downloads.
    "OLLAMA_HOST",
    "HF_ENDPOINT",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // Windows: what the system and the `dirs` crate need.
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERNAME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
];

/// Prefixes a spawned daemon keeps: the user's engramdb settings (data,
/// config, registry and model-cache paths, offline mode), locale categories,
/// and the XDG base directories every path in `engram_storage::paths`
/// resolves through.
const DAEMON_ENV_KEEP_PREFIXES: &[&str] = &["ENGRAMDB_", "LC_", "XDG_"];

/// The environment a spawned daemon starts with: the entries of `vars` that
/// [`DAEMON_ENV_KEEP`] or [`DAEMON_ENV_KEEP_PREFIXES`] name.
///
/// `ENGRAMDB_IN_PROCESS` is dropped: it is one session's opt-out from the
/// daemon, not a setting of the daemon.
fn daemon_env(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vars.into_iter()
        .filter(|(name, _)| {
            let Some(name) = name.to_str() else {
                return false;
            };
            let upper = name.to_ascii_uppercase();
            upper != "ENGRAMDB_IN_PROCESS"
                && (DAEMON_ENV_KEEP.contains(&upper.as_str())
                    || DAEMON_ENV_KEEP_PREFIXES
                        .iter()
                        .any(|p| upper.starts_with(p)))
        })
        .collect()
}

#[cfg(test)]
mod spawn_tests {
    use super::{daemon_env, spawn_command};
    use std::path::Path;

    fn args(cmd: &tokio::process::Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// An automatic spawn passes no idle timeout: the daemon takes it from
    /// the global config, never from the project that spawned it.
    #[test]
    fn automatic_spawn_passes_no_idle_timeout() {
        let cmd = spawn_command(Path::new("/bin/engramdb"), Path::new("/run/d.sock"), None);
        assert_eq!(args(&cmd), ["daemon", "run", "--socket", "/run/d.sock"]);
    }

    /// `daemon restart --idle-timeout N` is an explicit user value and is
    /// still forwarded.
    #[test]
    fn explicit_override_is_forwarded() {
        let cmd = spawn_command(
            Path::new("/bin/engramdb"),
            Path::new("/run/d.sock"),
            Some(90),
        );
        assert_eq!(
            args(&cmd),
            [
                "daemon",
                "run",
                "--socket",
                "/run/d.sock",
                "--idle-timeout",
                "90"
            ]
        );
    }

    fn names(vars: &[(&str, &str)]) -> Vec<String> {
        daemon_env(vars.iter().map(|(k, v)| (k.into(), v.into())))
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    /// The spawning session's own variables never reach the daemon; the
    /// user's settings, paths and runtime location do.
    #[test]
    fn spawned_daemon_keeps_only_the_allowlist() {
        let kept = names(&[
            ("HOME", "/home/u"),
            ("PATH", "/usr/bin"),
            ("ENGRAMDB_DATA_DIR", "/d"),
            ("ENGRAMDB_IN_PROCESS", "1"),
            ("XDG_RUNTIME_DIR", "/run/user/1"),
            ("LC_ALL", "C"),
            ("ORT_DYLIB_PATH", "/lib/ort.so"),
            ("CLAUDE_CONFIG_DIR", "/c"),
            ("https_proxy", "http://p"),
            ("GIT_DIR", "/elsewhere/.git"),
            ("ANTHROPIC_API_KEY", "secret"),
            ("CLAUDE_PROJECT_DIR", "/p"),
            ("EDITOR", "vim"),
        ]);
        assert_eq!(
            kept,
            [
                "HOME",
                "PATH",
                "ENGRAMDB_DATA_DIR",
                "XDG_RUNTIME_DIR",
                "LC_ALL",
                "ORT_DYLIB_PATH",
                "CLAUDE_CONFIG_DIR",
                "https_proxy"
            ]
        );
    }

    /// A process started from the spawn command sees the allowlist and
    /// nothing else of this process's environment. Runs a stand-in "binary"
    /// that dumps its environment next to the socket path it is given.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawned_process_starts_without_the_spawners_environment() {
        use std::os::unix::fs::PermissionsExt;
        // nextest runs each test in its own process, so this is not shared.
        std::env::set_var("ENGRAMDB_SPAWN_TEST_KEEP", "kept");
        std::env::set_var("SPAWN_TEST_SESSION_SECRET", "leaked");
        let dir = tempfile::TempDir::new().unwrap();
        let exe = dir.path().join("fake-engramdb");
        std::fs::write(&exe, "#!/bin/sh\nenv > \"$4.env\"\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = dir.path().join("d.sock");

        let status = spawn_command(&exe, &socket, None).status().await.unwrap();
        assert!(status.success());
        let env = std::fs::read_to_string(dir.path().join("d.sock.env")).unwrap();
        assert!(env.contains("ENGRAMDB_SPAWN_TEST_KEEP=kept"), "{env}");
        assert!(!env.contains("SPAWN_TEST_SESSION_SECRET"), "{env}");
        assert!(!env.contains("CARGO_"), "{env}");
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
