use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn projects_info() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

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

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "list"])
        .assert()
        .success();
}

#[test]
fn projects_stats() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "stats"])
        .assert()
        .success();
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
