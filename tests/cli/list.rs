use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn list_empty_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "list"])
        .assert()
        .success();
}

#[test]
fn list_populated_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Use Rust"));
}

#[test]
fn list_filter_by_type() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "-t",
            "decision",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Use Rust"));
}

#[test]
fn list_filter_by_tags() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Tagged for filter",
            "-c",
            "Content",
            "--tags",
            "filterable",
        ],
    );
    helpers::add_memory(dir.path(), "decision", "Untagged", "No tags here");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--tags",
            "filterable",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tagged for filter"));
}

#[test]
fn list_sort_by_criticality() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--sort",
            "criticality",
        ])
        .assert()
        .success();
}

#[test]
fn list_sort_by_created() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--sort",
            "created",
        ])
        .assert()
        .success();
}

#[test]
fn list_sort_by_type() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--sort",
            "type",
        ])
        .assert()
        .success();
}

#[test]
fn list_reverse_order() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--sort",
            "criticality",
            "--reverse",
        ])
        .assert()
        .success();
}

#[test]
fn list_with_limit() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--limit",
            "1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
}

#[test]
fn list_json_output_valid() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "list"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should be valid JSON (array or object)
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "list --json should produce valid JSON: {}",
        stdout
    );
}

#[test]
fn list_filter_by_status() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "-s",
            "active",
        ])
        .assert()
        .success();
}

#[test]
fn list_filter_by_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Scoped memory",
            "-c",
            "Scoped content",
            "-l",
            "app.core",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--scope",
            "app.core",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scoped memory"));
}

#[test]
fn list_multiple_type_filters() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path()); // adds decision, convention, hazard

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "-t",
            "decision",
            "-t",
            "hazard",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("decision")
            || stdout.contains("hazard")
            || stdout.contains("Use Rust")
            || stdout.contains("Avoid unwrap"),
        "Expected to see decision or hazard memories: {}",
        stdout
    );
}

#[test]
fn list_sort_by_updated() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--sort",
            "updated",
        ])
        .assert()
        .success();
}

#[test]
fn list_scope_filter_no_match() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--scope",
            "nonexistent.scope.xyz",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should not contain any of the seeded memories
    assert!(
        !stdout.contains("Use Rust"),
        "Should not match any memories with nonexistent scope: {}",
        stdout
    );
}
