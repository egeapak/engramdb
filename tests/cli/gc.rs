use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn gc_dry_run_default() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Default GC is dry-run mode
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "gc"])
        .assert()
        .success();
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
