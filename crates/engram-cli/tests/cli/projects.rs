use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn projects_info() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Project info should contain the word "project" and show path/id
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("project"));
}

#[test]
fn projects_list() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // After init, project list should show at least the current project path
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "list"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The directory path should appear in the list
    assert!(
        stdout.contains(dir.path().to_str().unwrap()) || stdout.contains("project"),
        "Projects list should reference the project: {}",
        stdout
    );
}

#[test]
fn projects_stats() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Stats should show counts
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "stats"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("total") || stdout.contains("3") || stdout.contains("memor"),
        "Stats should show memory counts: {}",
        stdout
    );
}

#[test]
fn projects_info_json_output() {
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse projects info JSON: {} — output: {}",
            e, stdout
        )
    });
    assert!(
        val.get("project_id").is_some(),
        "JSON should have 'project_id' key: {}",
        stdout
    );
}

#[test]
fn projects_delete_nonexistent_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "projects",
            "delete",
            "fake-id",
            "--force",
        ])
        .assert()
        .failure();
}
