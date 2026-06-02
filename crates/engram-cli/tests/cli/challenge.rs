use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn challenge_basic() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(
        dir.path(),
        "decision",
        "Challengeable",
        "Content to challenge",
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "challenge",
            &id,
            "--evidence",
            "This is no longer valid because of new requirements",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Challenged memory"));
}

#[test]
fn challenge_with_source_file() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(
        dir.path(),
        "convention",
        "Outdated convention",
        "Old convention",
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "challenge",
            &id,
            "--evidence",
            "Updated in source",
            "--source-file",
            "src/main.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Challenged memory"));
}

#[test]
fn challenge_changes_status() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Will be challenged", "Content");

    // Challenge the memory
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "challenge",
            &id,
            "--evidence",
            "Evidence against this",
        ])
        .assert()
        .success();

    // Verify status changed via get
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id, "--full"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        stdout.contains("challenged") || stdout.contains("challenge"),
        "expected 'challenged' or 'challenge' in output: {}",
        stdout
    );
}

#[test]
fn challenge_nonexistent_memory_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "challenge",
            "nonexistent-id-that-does-not-exist",
            "--evidence",
            "Evidence",
        ])
        .assert()
        .failure();
}
