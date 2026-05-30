//! Shared embedding daemon.
//!
//! stdio MCP is one process per agent session, so without coordination every
//! concurrent session loads its own copy of the embedding (and optional NLI /
//! reranker) models — hundreds of MB and a ~240ms ONNX init each. This module
//! provides a single long-lived daemon that loads each model once and serves
//! inference to every MCP process over a Unix domain socket.
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

pub use client::{query_status, request_shutdown, DaemonHandle};
pub use doctor::check_daemon;
pub use protocol::{DaemonStatus, PROTOCOL_VERSION};
pub use remote::remote_providers;
pub use server::run_daemon;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

/// Base directory for the daemon's socket and lock files.
///
/// Prefers `$XDG_RUNTIME_DIR` (tmpfs, per-user, cleared on logout), then the
/// per-user cache dir, then the system temp dir. Per-user by construction so
/// daemons of different users never collide.
fn runtime_base() -> PathBuf {
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("engramdb")
}

/// The default per-user socket path (no overrides applied).
fn default_socket_path() -> PathBuf {
    runtime_base().join("daemon.sock")
}

/// Path to the daemon's Unix domain socket, applying env + default only.
///
/// Overridable via `ENGRAMDB_DAEMON_SOCKET` so tests (and unusual setups where
/// the default path would exceed the ~104-byte `sun_path` limit) can relocate
/// it. Prefer [`resolve_socket`] where a config is available.
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
