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

    // A freshly initialized, healthy store must exit 0: advisory findings
    // (binary not on PATH, untracked fingerprint, .mcp.json not configured)
    // render as warnings and no longer gate the exit code, so the outcome is
    // deterministic.
    assert!(
        output.status.success(),
        "fresh-init doctor must exit 0: status={:?} stderr={}",
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
    //
    // A fresh, healthy store exits 0: hard checks all pass and the formerly
    // "host-dependent" findings (binary not on PATH, untracked fingerprint)
    // are advisory warnings now, so the exit code is deterministic.
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

    // A fresh, healthy store exits 0 (see the pretty-format test above):
    // advisory findings render as warnings and do not gate the exit code.
    assert!(
        output.status.success(),
        "fresh-init doctor must exit 0: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
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
    // so we know both DoctorCommand variants land. A healthy store must
    // exit 0 so scripts/CI can gate on `doctor store`.
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "doctor", "store"])
        .assert()
        .success();
}

#[test]
fn doctor_fix_non_tty_without_yes_lists_fixes_and_exits_zero() {
    // An uninitialized project has one fixable issue (init). With --fix but no
    // --yes in a non-TTY context (assert_cmd pipes stdout), nothing is applied
    // — the fixes are listed and the command exits 0 instead of erroring.
    let dir = TempDir::new().unwrap();

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "plain",
            "doctor",
            "--fix",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "doctor --fix without --yes must not error in non-TTY: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("fixable issue") && combined.contains("--fix --yes"),
        "must list fixes and point at --yes; got: {combined}"
    );
}

#[test]
fn doctor_fix_yes_on_uninitialized_initializes_project() {
    // --fix --yes applies the safe fixes without prompting; for an
    // uninitialized project that means running init, after which .engramdb
    // exists.
    let dir = TempDir::new().unwrap();

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "plain",
            "doctor",
            "--fix",
            "--yes",
        ])
        .assert()
        .success();

    assert!(
        dir.path().join(".engramdb").join("manifest.toml").exists(),
        "doctor --fix --yes should have initialized the project"
    );
}

#[test]
fn doctor_validate_subcommand_runs() {
    // `doctor validate` loads each downloaded model and test-infers. We don't
    // assert pass/fail (that depends on which models are cached on the runner),
    // only that the subcommand wires up and reports per-model results.
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "plain",
            "doctor",
            "validate",
        ])
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("Embedding model"),
        "validate must report the embedding model; got: {combined}"
    );
}

#[test]
fn doctor_store_unhealthy_exits_nonzero() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Plant an orphaned memory file (on disk but not in the index): the
    // store is now unhealthy, so `doctor store` must exit non-zero — exit 0
    // on a broken store would make CI gating on doctor useless.
    let orphan = dir
        .path()
        .join(".engramdb")
        .join("memories")
        .join("orphan-001.md");
    std::fs::write(&orphan, "---\nid: orphan-001\n---\n").unwrap();

    // (The "N orphaned files" warning goes to stderr in the piped/JSON
    // output mode; the orphan IDs themselves are listed on stdout.)
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "doctor", "store"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("orphan-001"));
}
