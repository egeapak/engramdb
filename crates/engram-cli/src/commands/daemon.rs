//! `engramdb daemon` subcommands: run / status / stop / restart.

use crate::app::DaemonCommand;
use crate::output::{format_ping_line, outln, OutputFormatter};
use anyhow::Result;
use engramdb::daemon;
use engramdb::types::DaemonConfig;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Point this process's stderr at the daemon log unless a terminal is
/// attached.
///
/// `engramdb daemon run` reaches here two ways. Typed by a human, stderr is a
/// terminal they are watching, and diverting to a file they would then have to
/// `tail` is strictly worse — so leave it alone. Started any other way (a
/// redirect, systemd, CI, or a supervisor), the output would otherwise go
/// nowhere, which is the failure this whole change exists to fix.
///
/// The redirect is done at the file-descriptor level (`dup2`) rather than by
/// reconfiguring `tracing`, because the subscriber is installed in `cli::run`
/// before any command dispatches and already holds stderr. Moving fd 2
/// redirects the subscriber, any library writing to stderr, and a panic
/// message alike — which is exactly the set worth capturing.
///
/// Best-effort: a failure here leaves stderr as it was.
#[cfg(unix)]
fn redirect_stderr_to_log_unless_tty(socket: &Path) {
    use engramdb::daemon::logging::{daemon_log_for_spawn, stderr_target, StderrTarget};
    use std::io::IsTerminal;

    if stderr_target(std::io::stderr().is_terminal()) == StderrTarget::Inherit {
        return;
    }
    let file = match daemon_log_for_spawn(socket) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("cannot open the daemon log ({e}); leaving stderr as-is");
            return;
        }
    };
    // `rustix` is the safe-syscall crate already used for this project's other
    // Unix work (the `geteuid`/`SO_PEERCRED` socket hardening).
    if let Err(e) = rustix::stdio::dup2_stderr(&file) {
        tracing::warn!("cannot redirect stderr to the daemon log ({e}); leaving stderr as-is");
    }
}

/// Non-Unix: `dup2` has no direct equivalent worth the complexity here, and
/// the auto-spawn path (which is what actually needs capture) redirects the
/// child's stderr at spawn time on every platform.
#[cfg(not(unix))]
fn redirect_stderr_to_log_unless_tty(_socket: &Path) {}

/// The `[daemon]` section of the global config (best effort; defaults when
/// absent). The daemon is one process shared by every project, so its
/// subcommands read the same global settings no matter which directory they
/// run in — a project's `.engramdb/config.toml` is never consulted.
async fn global_daemon_config() -> DaemonConfig {
    engramdb::storage::config::load_global_config_or_default()
        .await
        .daemon
}

/// The socket and idle timeout `daemon run` serves with: the `--socket` /
/// `--idle-timeout` flags when given (tests and hand-run daemons use them),
/// else the global config. An auto-spawned daemon gets only `--socket`, so its
/// lifetime is always the global `idle_timeout_secs`.
fn run_settings(
    socket_flag: Option<&Path>,
    idle_flag: Option<u64>,
    global: &DaemonConfig,
) -> (PathBuf, Duration) {
    let socket = daemon::resolve_socket(socket_flag, global);
    let idle = Duration::from_secs(idle_flag.unwrap_or(global.idle_timeout_secs));
    (socket, idle)
}

fn fmt_dur(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Dispatch an `engramdb daemon <sub>` invocation.
pub async fn run_daemon_cmd(command: DaemonCommand, formatter: &OutputFormatter) -> Result<()> {
    let cfg = global_daemon_config().await;
    match command {
        DaemonCommand::Run {
            socket,
            idle_timeout,
        } => {
            let (socket, idle) = run_settings(socket.as_deref(), idle_timeout, &cfg);
            redirect_stderr_to_log_unless_tty(&socket);
            daemon::run_daemon(socket, idle).await
        }

        DaemonCommand::Status { socket } => {
            let socket = daemon::resolve_socket(socket.as_deref(), &cfg);
            match daemon::query_status(&socket).await? {
                None if formatter.is_json() => {
                    // One JSON object (mirrors stats --daemon), not two
                    // {"message": ...} lines a script can't dispatch on.
                    outln!(
                        formatter,
                        "{}",
                        serde_json::json!({
                            "running": false,
                            "socket": socket.display().to_string(),
                        })
                    );
                }
                None => {
                    formatter.print_message(&format!(
                        "Daemon: not running (socket {})",
                        socket.display()
                    ));
                    formatter.print_message(
                        "It is auto-spawned on demand by the next MCP run when [daemon] is enabled.",
                    );
                }
                Some(s) if formatter.is_json() => {
                    // One JSON object so scripted consumers get parseable output
                    // instead of print_success's JSON followed by raw text lines
                    // (finding #7).
                    outln!(
                        formatter,
                        "{}",
                        crate::output::daemon_status_json(&s, &socket)
                    );
                }
                Some(s) => {
                    formatter.print_success(&format!("Daemon: running (pid {})", s.pid));
                    outln!(formatter, "  socket:          {}", socket.display());
                    outln!(formatter, "  protocol:        v{}", s.version);
                    outln!(formatter, "  uptime:          {}", fmt_dur(s.uptime_secs));
                    outln!(formatter, "  idle:            {}", fmt_dur(s.idle_secs));
                    outln!(formatter, "  model bundles:   {}", s.bundles_loaded);
                    outln!(
                        formatter,
                        "  {}",
                        format_ping_line(s.ping_count, s.last_ping_secs_ago)
                    );
                    outln!(formatter, "  requests (cumulative across restarts):");
                    outln!(formatter, "    embed:         {}", s.requests.embed);
                    outln!(formatter, "    classify:      {}", s.requests.classify);
                    outln!(formatter, "    rerank:        {}", s.requests.rerank);
                    outln!(formatter, "    meta:          {}", s.requests.meta);
                    outln!(formatter, "    status:        {}", s.requests.status);
                    outln!(formatter, "    title:         {}", s.requests.title);
                    outln!(formatter, "    total:         {}", s.requests.total);
                }
            }
            Ok(())
        }

        DaemonCommand::Stop { socket } => {
            let socket = daemon::resolve_socket(socket.as_deref(), &cfg);
            if daemon::request_shutdown(&socket).await? {
                formatter.print_success("Daemon: shutdown requested");
            } else {
                formatter.print_message("Daemon: not running");
            }
            Ok(())
        }

        DaemonCommand::Restart {
            socket,
            idle_timeout,
        } => {
            let socket = daemon::resolve_socket(socket.as_deref(), &cfg);
            let was_running = daemon::request_shutdown(&socket).await?;
            if was_running {
                // Wait for the old daemon to release the socket before
                // spawning a fresh one, so we don't reconnect to the dying
                // process.
                for _ in 0..40 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    if daemon::query_status(&socket).await?.is_none() {
                        break;
                    }
                }
            }
            // Only an explicit `--idle-timeout` is forwarded; without it the
            // new daemon reads the global config like any auto-spawned one.
            match daemon::DaemonHandle::connect_or_spawn(socket.clone(), idle_timeout).await {
                Some(_) => {
                    let verb = if was_running { "restarted" } else { "started" };
                    match daemon::query_status(&socket).await? {
                        Some(s) => {
                            formatter.print_success(&format!("Daemon: {verb} (pid {})", s.pid))
                        }
                        None => formatter.print_success(&format!("Daemon: {verb}")),
                    }
                    Ok(())
                }
                None => {
                    formatter.print_error("Daemon: failed to start");
                    anyhow::bail!("could not start daemon")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use tempfile::TempDir;

    fn fmt() -> OutputFormatter {
        OutputFormatter::new(Some(OutputFormat::Json), false, false)
    }

    #[test]
    fn fmt_dur_uses_largest_unit() {
        // The fmt_dur switch is one of the simpler branches inside this
        // file, but it has 4 cases and zero direct tests today.
        assert_eq!(fmt_dur(0), "0s");
        assert_eq!(fmt_dur(45), "45s");
        assert_eq!(fmt_dur(125), "2m 5s");
        assert_eq!(fmt_dur(3725), "1h 2m 5s");
        assert_eq!(fmt_dur(90_061), "1d 1h 1m");
    }

    /// `daemon status` against a socket no daemon owns: must print
    /// "not running" and return Ok. This is the larger of the two
    /// Status branches at run_daemon_cmd:51-80.
    #[tokio::test]
    async fn run_daemon_cmd_status_with_missing_socket_is_graceful() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("no-such.sock");

        let cmd = DaemonCommand::Status {
            socket: Some(socket),
        };
        // Result must be Ok and must not panic.
        run_daemon_cmd(cmd, &fmt()).await.unwrap();
    }

    /// The pretty "not running" branch (the `fmt()` helper above is JSON, so
    /// the graceful test covers the single-JSON-object arm instead).
    #[tokio::test]
    async fn run_daemon_cmd_status_missing_socket_pretty_mode() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("no-such-pretty.sock");

        let formatter = OutputFormatter::new(Some(crate::app::OutputFormat::Plain), false, true);
        let cmd = DaemonCommand::Status {
            socket: Some(socket),
        };
        run_daemon_cmd(cmd, &formatter).await.unwrap();
    }

    /// `daemon stop` against a socket no daemon owns: must print
    /// "not running" and return Ok. Exercises the False branch of the
    /// `request_shutdown` check at run_daemon_cmd:84.
    #[tokio::test]
    async fn run_daemon_cmd_stop_with_missing_socket_is_graceful() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("no-such-stop.sock");

        let cmd = DaemonCommand::Stop {
            socket: Some(socket),
        };
        run_daemon_cmd(cmd, &fmt()).await.unwrap();
    }

    /// `daemon run` without flags takes its idle timeout and socket from the
    /// global config file — the same file every session's heartbeat reads.
    #[tokio::test]
    async fn daemon_run_takes_idle_timeout_from_the_global_config() {
        let path = engramdb::storage::paths::global_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[daemon]\nidle_timeout_secs = 777\nsocket_path = \"/run/g/d.sock\"\n",
        )
        .unwrap();
        // This process's env may carry a socket override (nextest does not
        // set one; guard anyway so the assertion means what it says).
        std::env::remove_var("ENGRAMDB_DAEMON_SOCKET");
        let global = global_daemon_config().await;
        let (socket, idle) = run_settings(None, None, &global);
        assert_eq!(idle, Duration::from_secs(777));
        assert_eq!(socket, PathBuf::from("/run/g/d.sock"));

        // The flags still win, for tests and hand-run daemons.
        let (socket, idle) = run_settings(Some(Path::new("/x/y.sock")), Some(61), &global);
        assert_eq!(idle, Duration::from_secs(61));
        assert_eq!(socket, PathBuf::from("/x/y.sock"));
    }
}
