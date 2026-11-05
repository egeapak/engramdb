//! `engramdb dashboard` as a binary: the two behaviours only a spawned
//! process shows — the exit code when the port is taken, and the single
//! JSON line `--format json` prints before it starts serving.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use tempfile::TempDir;

/// The binary with every global location pointed at `home`.
fn engramdb(home: &TempDir) -> Command {
    #[allow(deprecated)]
    let bin = assert_cmd::cargo::cargo_bin("engramdb");
    let mut c = Command::new(bin);
    let h = home.path();
    c.env("ENGRAMDB_DATA_DIR", h.join("data"))
        .env("ENGRAMDB_CONFIG_DIR", h.join("config"))
        .env("ENGRAMDB_REGISTRY_PATH", h.join("registry.json"))
        .env("ENGRAMDB_DASHBOARD_CACHE_DIR", h.join("layouts"))
        .env("ENGRAMDB_IN_PROCESS", "1")
        .env("RUST_LOG", "off")
        .env("RUST_BACKTRACE", "0");
    c
}

#[test]
fn a_taken_port_exits_1_with_the_reason() {
    let home = TempDir::new().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let out = engramdb(&home)
        .args(["--dir", home.path().to_str().unwrap()])
        .args(["dashboard", "--no-open", "--port", &port])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot listen on"), "{err}");
    assert!(out.stdout.is_empty(), "nothing printed before the failure");
}

#[test]
fn json_output_is_one_line_with_the_fragment_url() {
    let home = TempDir::new().unwrap();
    let mut child = engramdb(&home)
        .args(["--dir", home.path().to_str().unwrap(), "--format", "json"])
        .args(["dashboard", "--no-open", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    child.kill().ok();
    child.wait().ok();
    let v: serde_json::Value = serde_json::from_str(line.trim()).expect("one JSON line");
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "a loopback URL");
    let (_, frag) = url.split_once("/#").expect("token in the fragment");
    assert!(frag.starts_with("token="), "token in the fragment");
    assert!(!url.contains('?'), "never in the query string");
    assert_eq!(v["allow_writes"], false);
    assert!(v["address"].as_str().unwrap().starts_with("127.0.0.1:"));
    assert_eq!(v["opened_browser"], false);
    let bare = v["address_url"].as_str().unwrap();
    assert!(bare.starts_with("http://127.0.0.1:") && !bare.contains("token"));
    assert!(
        v["current_project"].is_null(),
        "started outside any project"
    );
}
