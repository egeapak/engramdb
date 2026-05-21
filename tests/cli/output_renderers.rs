//! Integration tests for output renderers and CLI command handlers.
//!
//! These tests target the highest-CRAP functions identified by `cargo crap`:
//! every cli command handler (`run_*`) and `OutputFormatter::print_*` method
//! has high cyclomatic complexity but landed near 0% coverage because the
//! existing integration suite covers the `add`/`query` flows (which both
//! require live embeddings the sandbox can't reach).
//!
//! Strategy: drive only commands that don't trigger embedding — `init
//! --no-embeddings`, `stats`, `list`, `projects {info,list,stats,clean}`,
//! `daemon {status,stop}`, `delete`, `doctor` — and assert on real stdout
//! via `assert_cmd::Command` so the pretty/plain/JSON rendering branches
//! actually execute under coverage.

use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

// =====================================================================
// `engramdb stats` — covers run_stats + print_stats_pretty/_plain +
// print_runtime_pretty + print_embeddings_status
// =====================================================================

#[test]
fn stats_pretty_renders_totals_and_embedding_status_for_empty_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "stats",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stats must succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Total Memories: 0"),
        "missing total: {stdout}"
    );
    assert!(stdout.contains("By Type"), "missing By Type section");
    assert!(stdout.contains("By Status"), "missing By Status section");
    assert!(stdout.contains("Expired: 0"), "missing Expired counter");
    assert!(
        stdout.contains("Average Criticality"),
        "missing avg criticality"
    );
    // The Embeddings line is rendered by print_embeddings_status (CRAP 70,
    // CC 20, previously 0% covered). After `init --no-embeddings` the model
    // isn't available so this branch produces the "Not available" message.
    assert!(
        stdout.contains("Embeddings:"),
        "print_embeddings_status output missing: {stdout}"
    );
}

#[test]
fn stats_plain_renders_without_ansi_escapes() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "plain",
            "stats",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Total"), "plain stats must contain Total");
    assert!(
        !stdout.contains("\x1b["),
        "plain output must not emit ANSI escapes: {stdout}"
    );
}

#[test]
fn stats_json_serializes_full_struct() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "stats"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // `stats --json` emits a JSON object followed by a plain-text
    // `Embeddings: ...` line (print_embeddings_status writes outside the
    // JSON envelope by design). Parse only the leading JSON object.
    let json_end = stdout.find("\n}\n").map(|i| i + 3).unwrap_or(stdout.len());
    let json_slice = &stdout[..json_end];
    let value: serde_json::Value = serde_json::from_str(json_slice)
        .unwrap_or_else(|e| panic!("stats --json must parse: {e}\nstdout: {stdout}"));
    // Lock the wire contract: every documented field of `Stats` is present.
    assert!(value.get("total").is_some());
    assert!(value.get("by_type").is_some());
    assert!(value.get("by_status").is_some());
    assert!(value.get("expired").is_some());
    assert!(value.get("avg_criticality").is_some());
}

// =====================================================================
// `engramdb projects info|list|stats|clean` — covers run_projects (CRAP
// 462) + print_project_info, print_project_list, print_aggregate_stats
// =====================================================================

#[test]
fn projects_info_pretty_shows_id_path_and_memory_count() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "projects",
            "info",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Project:"), "missing Project: header");
    assert!(stdout.contains("ID:"), "missing ID:");
    assert!(stdout.contains("Path:"), "missing Path:");
    assert!(stdout.contains("Memories: 0"), "missing memory count");
    assert!(stdout.contains("Created:"), "missing Created:");
}

#[test]
fn projects_info_json_round_trips() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "projects",
            "info",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(v["project_id"].is_string(), "project_id field missing");
    assert!(v["project_name"].is_string(), "project_name field missing");
    assert!(v["memory_count"].is_number(), "memory_count field missing");
    // Optional field is omitted when None.
    assert!(v.get("parent_project_id").is_none());
}

#[test]
fn projects_list_pretty_shows_at_least_current_project() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "projects",
            "list",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let path = dir.path().to_string_lossy().to_string();
    assert!(
        stdout.contains(&path) || stdout.contains("No registered"),
        "expected current project path or empty marker: {stdout}"
    );
}

#[test]
fn projects_list_json_is_array() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "projects",
            "list",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let arr = v.as_array().expect("projects list --json must be array");
    if !arr.is_empty() {
        // Every entry has project_id, project_path, exists.
        let first = &arr[0];
        assert!(first["project_id"].is_string());
        assert!(first["project_path"].is_string());
        assert!(first["exists"].is_boolean());
    }
}

#[test]
fn projects_stats_aggregate_renders() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // `projects stats` renders via OutputFormatter::print_aggregate_stats
    // (CRAP 56, was 0% covered).
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "projects",
            "stats",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Total Projects"), "missing total projects");
    assert!(
        stdout.contains("Reachable"),
        "missing Reachable line: {stdout}"
    );
    assert!(stdout.contains("Total Memories"), "missing total memories");
}

#[test]
fn projects_stats_json_includes_counts() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "projects",
            "stats",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(v["total_projects"].is_number());
    assert!(v["reachable_projects"].is_number());
    assert!(v["total_memories"].is_number());
    assert!(v["by_type"].is_array());
}

// =====================================================================
// `engramdb daemon status|stop` — covers run_daemon_cmd (CRAP 462)
//
// We exercise the no-daemon-running branches: a fresh socket path that
// no daemon owns. The Run subcommand spawns the event loop and is left
// alone here — it's covered by daemon::tests in-process.
// =====================================================================

#[test]
fn daemon_status_with_no_running_daemon_reports_not_running() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("nonexistent.sock");

    let output = helpers::cmd()
        .args(["daemon", "status", "--socket", socket.to_str().unwrap()])
        .env("ENGRAMDB_DAEMON_SOCKET", &socket)
        .output()
        .unwrap();

    assert!(output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("not running")
            || combined.contains("Not running")
            || combined.contains("\"message\""),
        "expected not-running message: {combined}"
    );
}

#[test]
fn daemon_stop_with_no_running_daemon_is_graceful() {
    let dir = TempDir::new().unwrap();
    let socket = dir.path().join("nonexistent.sock");

    // `daemon stop` against a missing daemon must not panic / non-zero;
    // it prints "Daemon: not running" (run_daemon_cmd::Stop branch).
    helpers::cmd()
        .args(["daemon", "stop", "--socket", socket.to_str().unwrap()])
        .env("ENGRAMDB_DAEMON_SOCKET", &socket)
        .assert()
        .success();
}

// =====================================================================
// `engramdb list` — exercises the empty-store and JSON branches of
// print_memory_list. The verbose list pretty branch needs a memory in
// the store, but `add` triggers embedding in this sandbox. We at least
// lock down the empty-store text and the JSON serialization shape.
// =====================================================================

#[test]
fn list_empty_store_pretty_reports_no_memories() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "list",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No memories"));
}

#[test]
fn list_empty_store_json_is_empty_array() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "list"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let arr = v.as_array().expect("list --json must be array");
    assert_eq!(arr.len(), 0);
}

// =====================================================================
// `engramdb delete` — exercises run_delete error-path with a missing id.
// =====================================================================

#[test]
fn delete_nonexistent_id_fails_cleanly() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "delete",
            "no-such-id",
            "--force",
        ])
        .output()
        .unwrap();

    // run_delete must surface the not-found error rather than panic. The
    // exit code is non-zero (it's an error condition), but the error
    // message must mention the id can't be found.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.to_lowercase().contains("not found")
            || combined.to_lowercase().contains("no memor"),
        "delete of unknown id should report not-found: {combined}"
    );
}

// =====================================================================
// `engramdb doctor` extra coverage — drive the embeddings-mismatch and
// uninitialized-store branches that the existing doctor.rs tests miss.
// =====================================================================

#[test]
fn doctor_environment_against_uninitialized_dir_runs() {
    // No `init` here — the doctor must still produce a usable report
    // (it has an "uninitialized" branch in store_check).
    let dir = TempDir::new().unwrap();

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "doctor",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("EngramDB Environment Check"));
    // The Project section's "Store initialized" check should report not-init.
    // We don't assert specific text — just that the report renders fully.
    assert!(stdout.contains("Project"), "Project section missing");
}
