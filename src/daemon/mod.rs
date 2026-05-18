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
pub mod protocol;
pub mod remote;
pub mod server;

pub use client::DaemonHandle;
pub use protocol::PROTOCOL_VERSION;
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

/// Path to the daemon's Unix domain socket.
///
/// Overridable via `ENGRAMDB_DAEMON_SOCKET` so tests (and unusual setups where
/// the default path would exceed the ~104-byte `sun_path` limit) can relocate
/// it. Clients and the daemon resolve this identically.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ENGRAMDB_DAEMON_SOCKET") {
        return PathBuf::from(p);
    }
    runtime_base().join("daemon.sock")
}
