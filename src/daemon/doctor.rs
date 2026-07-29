//! Daemon health probe for the environment doctor.
//!
//! This lives in the `daemon` module (which may depend on `ops`) rather than in
//! `ops::doctor`, so that `ops` does not depend "upward" on `daemon`. The CLI
//! layer — which depends on both — injects the resulting [`EnvironmentCheck`]
//! into [`crate::ops::doctor_environment`].

use crate::ops::{CheckStatus, EnvironmentCheck};
use std::path::Path;

/// How many trailing daemon-log lines to surface. Enough to carry a panic
/// message plus its context, short enough not to bury the rest of `doctor`.
const LOG_TAIL_LINES: usize = 10;

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

    // Attach the tail of the daemon log whenever there is one. An
    // auto-spawned daemon is detached, so this file is the only record of why
    // it failed; surfacing it here is what turns "the daemon isn't running"
    // into an actionable diagnosis.
    let log_path = crate::storage::paths::daemon_log_path().ok();
    if let Some(path) = &log_path {
        let tail = super::logging::tail(path, LOG_TAIL_LINES);
        if !tail.is_empty() {
            details.push(format!("log: {}", path.display()));
            details.extend(tail.into_iter().map(|l| format!("  {l}")));
        }
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
    use tempfile::TempDir;

    /// Point `[daemon].socket_path` at `socket` inside a throwaway project.
    fn project_with_socket(socket: &Path) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let engram = tmp.path().join(".engramdb");
        std::fs::create_dir_all(&engram).unwrap();
        std::fs::write(
            engram.join("config.toml"),
            format!(
                "[daemon]\nenabled = true\nsocket_path = {:?}\n",
                socket.display().to_string()
            ),
        )
        .unwrap();
        tmp
    }

    #[tokio::test]
    async fn occupied_socket_with_no_daemon_is_a_failure() {
        // The pathological case: something owns the path but will not serve.
        // Reporting this as "auto-spawned on the next MCP run" is wrong — the
        // next run hits the same obstruction, forever.
        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("d.sock");
        std::fs::write(&socket, b"not a socket").unwrap();
        let project = project_with_socket(&socket);

        let check = check_daemon(project.path()).await;

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
        let project = project_with_socket(&socket);

        let check = check_daemon(project.path()).await;

        assert!(check.passed, "a cold start is not a failure");
        assert_eq!(check.status, Some(CheckStatus::Info));
        assert!(check.message.contains("not running"));
    }

    #[tokio::test]
    async fn disabled_daemon_short_circuits_before_the_socket_probe() {
        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("d.sock");
        // Occupied *and* disabled: config wins, so this must not be a failure.
        std::fs::write(&socket, b"not a socket").unwrap();
        let tmp = TempDir::new().unwrap();
        let engram = tmp.path().join(".engramdb");
        std::fs::create_dir_all(&engram).unwrap();
        std::fs::write(
            engram.join("config.toml"),
            format!(
                "[daemon]\nenabled = false\nsocket_path = {:?}\n",
                socket.display().to_string()
            ),
        )
        .unwrap();

        let check = check_daemon(tmp.path()).await;

        assert!(check.passed);
        assert_eq!(check.status, Some(CheckStatus::Info));
        assert!(check.message.contains("disabled"));
    }

    #[tokio::test]
    async fn the_daemon_log_tail_is_surfaced() {
        // The log is the only record of why a detached daemon died, so the
        // check has to show it — otherwise the user is told something is
        // wrong but not what.
        let log = crate::storage::paths::daemon_log_path().unwrap();
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "error: ORT dylib not found at /nope\n").unwrap();

        let sock_dir = TempDir::new().unwrap();
        let socket = sock_dir.path().join("d.sock");
        let project = project_with_socket(&socket);

        let check = check_daemon(project.path()).await;

        assert!(
            check
                .details
                .iter()
                .any(|d| d.contains("ORT dylib not found")),
            "the daemon log tail must appear in the check details: {:?}",
            check.details
        );
        let _ = std::fs::remove_file(&log);
    }
}
