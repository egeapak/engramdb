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
pub mod metrics;
pub mod protocol;
pub mod remote;
pub mod server;
// Platform IPC transport: Unix domain sockets on Unix, named pipes on Windows.
// Both `server` and `client` go through this so the daemon has the same
// capability on every platform.
mod transport;

pub use client::{query_status, request_shutdown, DaemonHandle};
pub use doctor::check_daemon;
pub use protocol::{DaemonStatus, PROTOCOL_VERSION};
pub use remote::remote_providers;
pub use server::run_daemon;

// The daemon tests drive the transport through the cross-platform `transport`
// seam, so they build and run on both Unix (domain sockets) and Windows (named
// pipes). The single Unix-domain-socket-specific case (a stale regular file at
// the socket path) is `#[cfg(unix)]`-gated within the module.
#[cfg(test)]
mod tests;

use std::path::PathBuf;

/// This process's effective uid. Used for the daemon's access-control
/// layers: per-uid default socket paths, socket-directory ownership checks,
/// and the `SO_PEERCRED` peer check in the accept loop.
#[cfg(unix)]
pub(crate) fn current_euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Base directory for the daemon's socket and lock files.
///
/// Prefers `$XDG_RUNTIME_DIR` (tmpfs, per-user, mode 0700 by spec, cleared on
/// logout), then the per-user cache dir, then — as a last resort on systems
/// with neither (cron, minimal containers, some su/sudo sessions) — a
/// **per-uid** subdirectory of the system temp dir (`/tmp/engramdb-<uid>`).
/// The uid in the temp-dir name guarantees two users never contend for the
/// same default path even under a shared world-writable `/tmp`.
///
/// The base alone is not the security boundary: whichever directory is chosen
/// (including config/env overrides), the Unix transport hardens it at bind
/// time — the socket's parent directory is created with (or tightened to)
/// mode 0700, the socket file is chmod'd 0600, and the server verifies each
/// accepted peer's uid via `SO_PEERCRED` (see [`transport`] and
/// `server::peer_allowed`). So even a fallback under `/tmp` is not reachable
/// by other local users.
fn runtime_base() -> PathBuf {
    if let Some(d) = dirs::runtime_dir().or_else(dirs::cache_dir) {
        return d.join("engramdb");
    }
    #[cfg(unix)]
    let leaf = format!("engramdb-{}", current_euid());
    #[cfg(not(unix))]
    let leaf = String::from("engramdb");
    std::env::temp_dir().join(leaf)
}

/// The default per-user socket path (no overrides applied).
fn default_socket_path() -> PathBuf {
    runtime_base().join("daemon.sock")
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
/// 3. `[daemon].socket_path` in config (`cfg`),
/// 4. the default per-user runtime path.
///
/// Clients, the MCP server, `doctor`, `stats`, and the daemon itself all
/// resolve identically so they agree on which socket a daemon lives at.
pub fn resolve_socket(cli: Option<&std::path::Path>, cfg: &crate::types::DaemonConfig) -> PathBuf {
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
