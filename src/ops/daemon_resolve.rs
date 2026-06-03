//! Shared daemon-or-in-process provider resolver.
//!
//! This module provides [`DaemonPolicy`], which expresses how a front-end may
//! obtain model providers, and (in later tasks) [`DaemonCell`], the
//! re-resolvable cell that backs both the MCP server and CLI.

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
