//! Integration tests for `engramdb doctor`.
//!
//! The CLI's `doctor` command renders an `EnvironmentDoctorResult` via
//! `OutputFormatter::print_environment_doctor`. The JSON branch goes
//! through `serde_json::to_string_pretty`; the pretty/plain branches go
//! through `println!`. Unit tests in `src/cli/output.rs` already lock
//! down the serde shape; these integration tests capture the real
//! stdout and assert it actually contains what we ship to users.

use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn doctor_environment_json_is_parseable() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "doctor"])
        .output()
        .unwrap();

    assert!(
        output.status.success() || !output.stdout.is_empty(),
        "doctor must not crash: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("doctor --json must emit parseable JSON; parse error: {e}\nstdout: {stdout}")
    });

    // The OutputFormatter::print_environment_doctor JSON path serializes
    // the EnvironmentDoctorResult directly: this is the wire contract that
    // external consumers depend on.
    assert!(
        value.get("sections").is_some(),
        "missing 'sections' field; got: {value}"
    );
    assert!(
        value.get("all_passed").is_some(),
        "missing 'all_passed' field"
    );
    let sections = value["sections"].as_array().unwrap();
    assert!(!sections.is_empty(), "sections array must be populated");

    // Every section has a name + checks array — locks down the shape.
    for s in sections {
        assert!(s["name"].is_string(), "section.name must be string");
        assert!(s["checks"].is_array(), "section.checks must be array");
    }
}

#[test]
fn doctor_environment_pretty_renders_section_headers() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // `--format pretty` forces the Pretty branch of print_environment_doctor;
    // without it, non-TTY stdout (assert_cmd pipes stdout) falls back to JSON
    // per OutputFormatter::new at output.rs:43-58.
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "doctor",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("EngramDB Environment Check"));
}

#[test]
fn doctor_environment_plain_renders_sections_without_color_codes() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "plain",
            "doctor",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EngramDB Environment Check"),
        "plain output must include the header"
    );
    // Plain format must not emit ANSI color escapes. \x1b[ is the CSI introducer.
    assert!(
        !stdout.contains("\x1b["),
        "plain format must strip ANSI escapes: {stdout}"
    );
}

#[test]
fn doctor_store_subcommand_runs() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // The fast store-only path is a separate subcommand — exercise it too
    // so we know both DoctorCommand variants land.
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "doctor", "store"])
        .assert()
        .success();
}
