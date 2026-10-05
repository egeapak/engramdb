//! Daemon health probe for the environment doctor.
//!
//! This lives in the `daemon` module (which may depend on `ops`) rather than in
//! `ops::doctor`, so that `ops` does not depend "upward" on `daemon`. The CLI
//! layer — which depends on both — injects the resulting [`EnvironmentCheck`]
//! into [`crate::ops::doctor_environment`].

use crate::ops::{CheckStatus, EnvironmentCheck};

/// How many trailing daemon-log lines to surface. Enough to carry a panic
/// message plus its context, short enough not to bury the rest of `doctor`.
const LOG_TAIL_LINES: usize = 10;

/// Inspect the shared embedding daemon: configured? reachable? Informational
/// only — the daemon is optional and auto-spawned by the next MCP run, so a
/// stopped daemon is never a failure.
///
/// Takes no project directory: the daemon's settings come only from the
/// global config file, whichever project `doctor` runs in. A project config
/// that still sets `[daemon]` keys is reported by the project's own config
/// check (`ops::doctor`), not here.
pub async fn check_daemon() -> EnvironmentCheck {
    let global_path = crate::storage::paths::global_config_path().ok();
    let config = match &global_path {
        Some(p) => match crate::storage::config::load_global_config(p).await {
            Ok(c) => c,
            Err(e) => {
                return EnvironmentCheck {
                    name: "Embedding daemon".to_string(),
                    passed: false,
                    message: format!("global config {} does not parse: {e}", p.display()),
                    suggestion: Some(format!(
                        "Fix the syntax in {}; until then every daemon setting is a default",
                        p.display()
                    )),
                    details: vec![],
                    status: Some(CheckStatus::Fail),
                };
            }
        },
        None => crate::types::GlobalConfig::default(),
    };
    let socket = super::resolve_socket(None, &config.daemon);
    let mut details = vec![
        format!("socket: {}", socket.display()),
        format!(
            "config: {} (enabled={}, idle_timeout_secs={})",
            global_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no global config dir>".to_string()),
            config.daemon.enabled,
            config.daemon.idle_timeout_secs
        ),
    ];
    if let Err(e) = config.validate() {
        details.push(format!("invalid global [daemon] value: {e}"));
    }

    if !config.daemon.enabled {
        return EnvironmentCheck {
            name: "Embedding daemon".to_string(),
            passed: true,
            message: "disabled in the global config (models load in-process per MCP)".to_string(),
            suggestion: None,
            details,
            status: Some(CheckStatus::Info),
        };
    }

    // Attach the tail of the daemon log whenever there is one. An
    // auto-spawned daemon is detached, so this file is the only record of why
    // it failed; surfacing it here is what turns "the daemon isn't running"
    // into an actionable diagnosis.
    let log_path = super::daemon_log_path(&socket);
    let tail = super::logging::tail(&log_path, LOG_TAIL_LINES);
    if !tail.is_empty() {
        details.push(format!("log: {}", log_path.display()));
        details.extend(tail.into_iter().map(|l| format!("  {l}")));
    }

    match super::query_status(&socket).await {
        Ok(Some(s)) => {
            details.push(format!(
                "pid {}, uptime {}s, {} model bundle(s), {} requests served (cumulative)",
                s.pid, s.uptime_secs, s.bundles_loaded, s.requests.total
            ));
            EnvironmentCheck {
                name: "Embedding daemon".to_string(),
                passed: true,
                message: format!("running (protocol v{})", s.version),
                suggestion: None,
                details,
                status: Some(CheckStatus::Info),
            }
        }
        // Nothing answered. Whether that is normal depends entirely on
        // whether the socket path is occupied: an absent path means no daemon
        // has been needed yet, which is fine and self-correcting. A path that
        // *exists* while nothing answers means the next spawn will hit the
        // same obstruction this one did — it is not self-correcting, and
        // reporting it as "auto-spawned on the next MCP run" is simply wrong.
        _ if socket.exists() => EnvironmentCheck {
            name: "Embedding daemon".to_string(),
            passed: false,
            message: "socket exists but no daemon answers — every spawn will keep failing"
                .to_string(),
            suggestion: Some(format!(
                "Stop any wedged daemon, then remove {} and run `engramdb daemon restart`.",
                socket.display()
            )),
            details,
            status: Some(CheckStatus::Fail),
        },
        _ => EnvironmentCheck {
            name: "Embedding daemon".to_string(),
            passed: true,
            message: "not running (auto-spawned on the next MCP run)".to_string(),
            suggestion: Some(
                "Run `engramdb daemon status` or `engramdb daemon restart`.".to_string(),
            ),
            details,
            status: Some(CheckStatus::Info),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// Write the global config (per-process temp dir under the test harness).
    fn write_global(toml: &str) {
        let path = crate::storage::paths::global_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, toml).unwrap();
    }

    /// Point the global `[daemon].socket_path` at `socket`.
    fn global_with_socket(socket: &Path, enabled: bool) {
        write_global(&format!(
            "[daemon]\nenabled = {enabled}\nsocket_path = {:?}\n",
            socket.display().to_string()
        ));
    }

    #[tokio::test]
    async fn occupied_socket_with_no_daemon_is_a_failure() {
        // The pathological case: something owns the path but will not serve.
        // Reporting this as "auto-spawned on the next MCP run" is wrong — the
        // next run hits the same obstruction, forever.
        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("d.sock");
        std::fs::write(&socket, b"not a socket").unwrap();
        global_with_socket(&socket, true);

        let check = check_daemon().await;

        assert!(
            !check.passed,
            "an occupied-but-dead socket must fail, got: {}",
            check.message
        );
        assert_eq!(check.status, Some(CheckStatus::Fail));
        assert!(
            check.suggestion.is_some_and(|s| s.contains("restart")),
            "a failing daemon check must say how to recover"
        );
    }

    #[tokio::test]
    async fn absent_socket_is_informational_not_a_failure() {
        // The control: no socket at all is the ordinary cold-start state and
        // really is self-correcting, so it must stay Info. Without this the
        // test above could be satisfied by failing on everything.
        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("never-created.sock");
        global_with_socket(&socket, true);

        let check = check_daemon().await;

        assert!(check.passed, "a cold start is not a failure");
        assert_eq!(check.status, Some(CheckStatus::Info));
        assert!(check.message.contains("not running"));
        let global = crate::storage::paths::global_config_path().unwrap();
        assert!(
            check
                .details
                .iter()
                .any(|d| d.contains(&global.display().to_string())),
            "the check must name the global config it read: {:?}",
            check.details
        );
    }

    #[tokio::test]
    async fn disabled_daemon_short_circuits_before_the_socket_probe() {
        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("d.sock");
        // Occupied *and* disabled: config wins, so this must not be a failure.
        std::fs::write(&socket, b"not a socket").unwrap();
        global_with_socket(&socket, false);

        let check = check_daemon().await;

        assert!(check.passed);
        assert_eq!(check.status, Some(CheckStatus::Info));
        assert!(check.message.contains("disabled"));
    }

    #[tokio::test]
    async fn an_unparseable_global_config_is_reported() {
        write_global("[daemon\n");
        let check = check_daemon().await;
        assert!(!check.passed);
        assert!(
            check.message.contains("does not parse"),
            "{}",
            check.message
        );
    }

    #[tokio::test]
    async fn the_daemon_log_tail_is_surfaced() {
        // The log is the only record of why a detached daemon died, so the
        // check has to show it — otherwise the user is told something is
        // wrong but not what. It lives in the daemon folder, beside the socket.
        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("d.sock");
        let log = crate::daemon::daemon_log_path(&socket);
        std::fs::write(&log, "error: ORT dylib not found at /nope\n").unwrap();
        global_with_socket(&socket, true);

        let check = check_daemon().await;

        assert!(
            check
                .details
                .iter()
                .any(|d| d.contains("ORT dylib not found")),
            "the daemon log tail must appear in the check details: {:?}",
            check.details
        );
    }
}
