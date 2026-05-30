//! Daemon health probe for the environment doctor.
//!
//! This lives in the `daemon` module (which may depend on `ops`) rather than in
//! `ops::doctor`, so that `ops` does not depend "upward" on `daemon`. The CLI
//! layer — which depends on both — injects the resulting [`EnvironmentCheck`]
//! into [`crate::ops::doctor_environment`].

use crate::ops::{CheckStatus, EnvironmentCheck};
use std::path::Path;

/// Inspect the shared embedding daemon: configured? reachable? Informational
/// only — the daemon is optional and auto-spawned by the next MCP run, so a
/// stopped daemon is never a failure.
pub async fn check_daemon(dir: &Path) -> EnvironmentCheck {
    let config = crate::storage::config::load_config(&dir.join(".engramdb").join("config.toml"))
        .await
        .unwrap_or_default();
    let socket = super::resolve_socket(None, &config.daemon);
    let mut details = vec![
        format!("socket: {}", socket.display()),
        format!(
            "config: enabled={}, idle_timeout_secs={}",
            config.daemon.enabled, config.daemon.idle_timeout_secs
        ),
    ];

    if !config.daemon.enabled {
        return EnvironmentCheck {
            name: "Embedding daemon".to_string(),
            passed: true,
            message: "disabled in config (models load in-process per MCP)".to_string(),
            suggestion: None,
            details,
            status: Some(CheckStatus::Info),
        };
    }

    let (message, suggestion) = match super::query_status(&socket).await {
        Ok(Some(s)) => {
            details.push(format!(
                "pid {}, uptime {}s, {} model bundle(s), {} requests served (cumulative)",
                s.pid, s.uptime_secs, s.bundles_loaded, s.requests_total
            ));
            (format!("running (protocol v{})", s.version), None)
        }
        _ => (
            "not running (auto-spawned on the next MCP run)".to_string(),
            Some("Run `engramdb daemon status` or `engramdb daemon restart`.".to_string()),
        ),
    };
    EnvironmentCheck {
        name: "Embedding daemon".to_string(),
        passed: true,
        message,
        suggestion,
        details,
        status: Some(CheckStatus::Info),
    }
}
