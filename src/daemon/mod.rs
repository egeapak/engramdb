//! Shared embedding daemon.
//!
//! stdio MCP is one process per agent session, so without coordination every
//! concurrent session loads its own copy of the embedding (and optional NLI /
//! reranker) models — hundreds of MB and a ~240ms ONNX init each. This module
//! provides a single long-lived daemon that loads each model once and serves
//! inference to every MCP process over a local IPC channel — a Unix domain
//! socket on Unix, a named pipe on Windows (see [`transport`]).
//!
//! MCP processes wire [`remote`] providers (behind the existing
//! `EmbeddingProvider` / `NliProvider` / `Reranker` trait seams) so storage
//! orchestration stays in the MCP while *all* model work is delegated. The
//! daemon is auto-spawned on demand, race-coordinated by an advisory file
//! lock, and exits after an idle period — a fresh one is spawned by the next
//! process that needs it. When the daemon is disabled in config or
//! unreachable, callers fall back to loading models in-process.

pub mod client;
pub mod doctor;
// Where a daemon's diagnostics go. An auto-spawned daemon is detached and
// outlives its parent, so its streams used to be discarded outright; this owns
// the log file that replaces `/dev/null` and the rule for when a hand-run
// daemon keeps its terminal instead.
pub mod logging;
pub mod metrics;
pub mod protocol;
pub mod remote;
// The shared daemon-or-in-process provider resolver (DaemonCell, DaemonPolicy,
// resolve_providers). Lives here rather than in `ops` because it necessarily
// reaches daemon internals (DaemonHandle, resolve_socket, remote_providers):
// keeping it in `ops` created the crate's only two-way module edge
// (ops <-> daemon). `daemon` may depend on `ops`, never the reverse.
pub mod resolve;
pub mod server;
// Platform IPC transport: Unix domain sockets on Unix, named pipes on Windows.
// Both `server` and `client` go through this so the daemon has the same
// capability on every platform.
mod transport;

pub use client::{query_status, request_shutdown, DaemonHandle};
pub use doctor::check_daemon;
pub use protocol::{DaemonStatus, ModelSelection, RequestCounts, PROTOCOL_VERSION};
pub use remote::remote_providers;
pub use resolve::{
    resolve_providers, resolve_providers_with, DaemonCell, DaemonPolicy, InProcessFallback,
};
pub use server::run_daemon;

// The daemon tests drive the transport through the cross-platform `transport`
// seam, so they build and run on both Unix (domain sockets) and Windows (named
// pipes). The single Unix-domain-socket-specific case (a stale regular file at
// the socket path) is `#[cfg(unix)]`-gated within the module.
#[cfg(test)]
mod tests;

/// A temp dir the daemon will accept as its folder. `tempfile` creates
/// directories with the umask default (usually 0755), and the transport
/// refuses any daemon folder that is not 0700, so every test that binds a
/// socket in a temp dir must use this instead of `TempDir::new()`.
#[cfg(test)]
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

use std::path::{Path, PathBuf};

/// This process's effective uid. Used for the daemon's access-control
/// layers: per-uid default socket paths, socket-directory ownership checks,
/// and the `SO_PEERCRED` peer check in the accept loop.
#[cfg(unix)]
pub(crate) fn current_euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The daemon's default folder: one shared, private, per-user directory that
/// holds the socket, the reclaim lock and `daemon.log`.
///
/// - `$XDG_RUNTIME_DIR/engramdb` when a runtime dir exists (tmpfs, per-user,
///   mode 0700 by spec, cleared on logout). Only the daemon uses it.
/// - Otherwise `<cache_dir>/engramdb/daemon`. This is a dedicated leaf rather
///   than `<cache_dir>/engramdb` itself, because that directory also holds the
///   model cache and is often created world-readable by a model download —
///   the daemon must own a folder whose mode it can insist on.
/// - As a last resort on systems with neither (cron, minimal containers, some
///   su/sudo sessions), a **per-uid** subdirectory of the system temp dir
///   (`/tmp/engramdb-<uid>`), so two users never contend for one path.
///
/// Whatever folder is used (including `--socket` / `ENGRAMDB_DAEMON_SOCKET` /
/// global `[daemon].socket_path` overrides), the Unix transport creates it
/// with mode 0700 and refuses to bind when it exists with another owner or a
/// looser mode (see [`transport`]). The server also checks each peer's uid via
/// `SO_PEERCRED` (`server::peer_allowed`).
pub fn default_daemon_dir() -> PathBuf {
    if let Some(d) = dirs::runtime_dir() {
        return d.join("engramdb");
    }
    if let Some(d) = dirs::cache_dir() {
        return d.join("engramdb").join("daemon");
    }
    #[cfg(unix)]
    let leaf = format!("engramdb-{}", current_euid());
    #[cfg(not(unix))]
    let leaf = String::from("engramdb");
    std::env::temp_dir().join(leaf)
}

/// The default per-user socket path (no overrides applied).
fn default_socket_path() -> PathBuf {
    default_daemon_dir().join("daemon.sock")
}

/// The daemon folder for a resolved `socket`: the directory that holds the
/// socket, the reclaim lock and `daemon.log`.
///
/// On Unix this is the socket's parent, so an overridden socket moves the
/// whole folder with it (tests and the eval runner rely on that for
/// isolation). A Windows named pipe has no parent directory, so an explicit
/// `\\.\pipe\...` name keeps its log in [`default_daemon_dir`].
pub fn daemon_dir_for(socket: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let lower = socket.to_string_lossy().to_ascii_lowercase();
        if lower.starts_with(r"\\.\pipe\") || lower.starts_with(r"\\?\pipe\") {
            return default_daemon_dir();
        }
    }
    match socket.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Path of the daemon's diagnostic log for a resolved `socket`:
/// `<daemon folder>/daemon.log`.
///
/// It lives beside the socket, not in the data dir, so the daemon has one
/// private folder and one folder to clean up. The cost: under
/// `$XDG_RUNTIME_DIR` the log is cleared on logout. That is acceptable for
/// this log, whose only job is "why did the daemon just fail".
pub fn daemon_log_path(socket: &Path) -> PathBuf {
    daemon_dir_for(socket).join("daemon.log")
}

/// Path to the daemon's IPC endpoint, applying env + default only.
///
/// On Unix this is the Unix-domain-socket path; on Windows [`transport`] maps it
/// to a named pipe. Overridable via `ENGRAMDB_DAEMON_SOCKET` so tests (and
/// unusual setups where the default path would exceed the ~104-byte Unix
/// `sun_path` limit) can relocate it. Prefer [`resolve_socket`] where a config
/// is available.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ENGRAMDB_DAEMON_SOCKET") {
        return PathBuf::from(p);
    }
    default_socket_path()
}

/// Resolve the daemon socket from the full override chain. Precedence,
/// highest first:
/// 1. an explicit `--socket` CLI flag (`cli`),
/// 2. the `ENGRAMDB_DAEMON_SOCKET` env var,
/// 3. `[daemon].socket_path` in the **global** config (`cfg`),
/// 4. the default per-user path ([`default_daemon_dir`]).
///
/// `cfg` must come from
/// [`crate::storage::config::load_global_config_or_default`], never from a
/// project config: two projects that resolved different sockets would spawn
/// two daemons and load every model twice. Clients, the MCP server, `doctor`,
/// `stats`, and the daemon itself all resolve identically so they agree on
/// which socket a daemon lives at.
pub fn resolve_socket(cli: Option<&Path>, cfg: &crate::types::DaemonConfig) -> PathBuf {
    if let Some(p) = cli {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("ENGRAMDB_DAEMON_SOCKET") {
        return PathBuf::from(p);
    }
    if let Some(p) = cfg.socket_path.as_deref().filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    default_socket_path()
}

/// The heartbeat interval for an idle timeout: `max(30s, idle/3)`.
///
/// Both sides of the daemon's lifetime read `idle_timeout_secs` from the same
/// global config — the daemon to decide when to exit, every session to decide
/// how often to ping — so `DaemonConfig::validate`'s `>= 60` floor really does
/// guarantee at least two pings per idle window. When the value came from each
/// project's own config, a session could ping slower than the daemon reaped.
pub fn heartbeat_interval(idle_timeout_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs((idle_timeout_secs / 3).max(30))
}
