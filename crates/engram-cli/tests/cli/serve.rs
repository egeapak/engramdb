//! `engramdb serve` over real stdio: what only the binary has (argument
//! handling, a relative `--dir`, the stdio transport with
//! `DiscoverProbeFilter`). The ordering of the handshake against the startup
//! work is held open deterministically by the in-crate MCP tests instead.

use crate::helpers::{init_store, std_cmd};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// The whole exchange must finish inside this (a debug binary on a shared
/// runner, cold).
const DEADLINE: Duration = Duration::from_secs(30);

/// A running `serve`, killed when dropped.
struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    deadline: Instant,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// `engramdb serve --dir .` started in `project`, as the shipped MCP
    /// entry runs it.
    fn start(project: &std::path::Path) -> Self {
        let mut c = std_cmd();
        c.current_dir(project)
            .args(["serve", "--dir", "."])
            // Startup's model check must not try a download.
            .env("ENGRAMDB_OFFLINE", "1")
            .env("RUST_LOG", "off")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().expect("spawn serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            deadline: Instant::now() + DEADLINE,
        }
    }

    /// Write `msg` and its newline in one write, as a pipe holds what a
    /// client sent while the server was not reading yet.
    fn send(&mut self, msg: &str) {
        self.stdin.write_all(format!("{msg}\n").as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// The next message from the server, within the deadline.
    fn recv(&mut self) -> serde_json::Value {
        let left = self.deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(left) {
            Ok(line) => {
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON-RPC ({e}): {line}"))
            }
            Err(RecvTimeoutError::Timeout) => panic!("serve did not answer within {DEADLINE:?}"),
            Err(RecvTimeoutError::Disconnected) => panic!("serve closed its stdout"),
        }
    }

    /// The response to request `id`, skipping notifications.
    fn response(&mut self, id: serde_json::Value) -> serde_json::Value {
        loop {
            let msg = self.recv();
            if msg.get("id") == Some(&id) {
                return msg;
            }
            assert!(
                msg.get("id").is_none(),
                "a response to another request arrived first: {msg}"
            );
        }
    }

    /// Nothing more arrives for `wait`.
    fn quiet_for(&mut self, wait: Duration) -> Option<String> {
        self.lines.recv_timeout(wait).ok()
    }
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
const STATS_CALL: &str =
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stats","arguments":{}}}"#;

fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    init_store(dir.path());
    dir
}

/// Handshake, list the tools, and run one tool after initialization; every
/// answer must come within the deadline.
fn handshake_and_call(s: &mut Server, init: serde_json::Value) {
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    s.send(INITIALIZED);
    s.send(TOOLS_LIST);
    let tools = s.response(serde_json::json!(1));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools: {tools}"))
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"query"), "{names:?}");
    s.send(STATS_CALL);
    let stats = s.response(serde_json::json!(2));
    assert!(stats["result"].is_object(), "stats failed: {stats}");
    assert_ne!(stats["result"]["isError"], true, "{stats}");
}

/// The shipped MCP entry is `serve --dir .`: a relative dir, which the
/// registry holds as a canonical absolute path (the case behind the
/// path-comparison deadlock).
#[test]
fn serve_with_a_relative_dir_answers_the_handshake() {
    let project = project();
    let mut s = Server::start(project.path());
    s.send(INITIALIZE);
    let init = s.response(serde_json::json!(0));
    handshake_and_call(&mut s, init);
}

/// A `server/discover` probe immediately followed by `initialize` (the
/// client gave up on the probe) gets no answer of its own, and the session
/// then works on the legacy lifecycle.
#[test]
fn serve_drops_an_abandoned_discover_probe() {
    let project = project();
    let mut s = Server::start(project.path());
    s.send(concat!(
        r#"{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#
    ));
    let first = s.recv();
    assert_eq!(
        first["id"], 0,
        "the abandoned probe was answered (or answered first): {first}"
    );
    handshake_and_call(&mut s, first);
    assert_eq!(
        s.quiet_for(Duration::from_millis(300))
            .filter(|l| l.contains("server-discover-probe-1")),
        None,
        "the probe was answered late"
    );
}
