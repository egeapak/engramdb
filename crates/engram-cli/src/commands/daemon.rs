//! `engramdb daemon` subcommands: run / status / stop / restart.

use crate::app::DaemonCommand;
use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::daemon;
use engramdb::types::DaemonConfig;
use std::path::PathBuf;
use std::time::Duration;

/// Load the `[daemon]` config from the current directory's store (best
/// effort; defaults when absent). Used so a configured `socket_path` /
/// `idle_timeout_secs` apply to the `daemon` subcommands too.
async fn cwd_daemon_config() -> DaemonConfig {
    let path = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".engramdb")
        .join("config.toml");
    engramdb::storage::config::load_config(&path)
        .await
        .unwrap_or_default()
        .daemon
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
    let cfg = cwd_daemon_config().await;
    match command {
        DaemonCommand::Run {
            socket,
            idle_timeout,
        } => {
            let socket = daemon::resolve_socket(socket.as_deref(), &cfg);
            let idle = Duration::from_secs(idle_timeout.unwrap_or(cfg.idle_timeout_secs));
            daemon::run_daemon(socket, idle).await
        }

        DaemonCommand::Status { socket } => {
            let socket = daemon::resolve_socket(socket.as_deref(), &cfg);
            match daemon::query_status(&socket).await? {
                None => {
                    formatter.print_message(&format!(
                        "Daemon: not running (socket {})",
                        socket.display()
                    ));
                    formatter.print_message(
                        "It is auto-spawned on demand by the next MCP run when [daemon] is enabled.",
                    );
                }
                Some(s) => {
                    formatter.print_success(&format!("Daemon: running (pid {})", s.pid));
                    println!("  socket:          {}", socket.display());
                    println!("  protocol:        v{}", s.version);
                    println!("  uptime:          {}", fmt_dur(s.uptime_secs));
                    println!("  idle:            {}", fmt_dur(s.idle_secs));
                    println!("  model bundles:   {}", s.bundles_loaded);
                    println!("  requests (cumulative across restarts):");
                    println!("    embed:         {}", s.requests_embed);
                    println!("    classify:      {}", s.requests_classify);
                    println!("    rerank:        {}", s.requests_rerank);
                    println!("    meta:          {}", s.requests_meta);
                    println!("    status:        {}", s.requests_status);
                    println!("    title:         {}", s.requests_title);
                    println!("    total:         {}", s.requests_total);
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
            let idle = idle_timeout.unwrap_or(cfg.idle_timeout_secs);
            match daemon::DaemonHandle::connect_or_spawn(socket.clone(), idle).await {
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
}
