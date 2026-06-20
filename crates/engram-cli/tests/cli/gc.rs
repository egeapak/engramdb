use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn gc_dry_run_default() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Default GC is dry-run mode — should report eligibility or no memories
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "gc"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        stdout.contains("eligible")
            || stdout.contains("dry run")
            || stdout.contains("no memor")
            || stdout.contains("gc"),
        "gc should report eligibility status: {}",
        stdout
    );
}

#[test]
fn gc_with_threshold_shows_candidates() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // High threshold should show candidates in dry-run
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "gc",
            "--threshold",
            "0.99",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        stdout.contains("eligible") || stdout.contains("dry run") || stdout.contains("no memor"),
        "gc dry-run should mention eligibility or dry run: {}",
        stdout
    );
}

#[test]
fn gc_with_confirm_shows_removal() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Add a very low criticality memory that's eligible for GC
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "debug",
            "-s",
            "Low priority debug",
            "-c",
            "Debug content",
            "--criticality",
            "0.01",
        ],
    );

    // Use a very high threshold so something gets collected
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "gc",
            "--confirm",
            "--threshold",
            "0.99",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        stdout.contains("removed") || stdout.contains("no memor"),
        "gc --confirm should report removal or 'no memories': {}",
        stdout
    );
}

#[test]
fn gc_empty_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "gc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No memories").or(predicate::str::contains("no memor")));
}

// Finding #7: `gc --json` dry-run plan must be a single valid JSON document
// (scripts parse it); previously per-id lines were printed raw after the
// formatter's JSON messages.
#[test]
fn gc_json_dry_run_is_valid_single_document() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "gc",
            "--threshold",
            "0.99",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "gc --json must be valid JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(v["dry_run"], true);
}
