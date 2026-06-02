use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn add_basic_memory() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Test summary",
            "-c",
            "Test content",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_all_memory_types() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let types = [
        "decision",
        "convention",
        "hazard",
        "context",
        "intent",
        "relationship",
        "debug",
        "preference",
    ];

    for type_ in &types {
        helpers::cmd()
            .args([
                "--dir",
                dir.path().to_str().unwrap(),
                "add",
                "-t",
                type_,
                "-s",
                &format!("Summary for {}", type_),
                "-c",
                &format!("Content for {}", type_),
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Created memory"));
    }
}

#[test]
fn add_with_tags() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "convention",
            "-s",
            "Tagged memory",
            "-c",
            "Content with tags",
            "--tags",
            "rust,style,important",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_with_physical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "convention",
            "-s",
            "Scoped memory",
            "-c",
            "Physical scope test",
            "-p",
            "src/**/*.rs",
        ])
        .assert()
        .success();
}

#[test]
fn add_with_logical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "convention",
            "-s",
            "Logical scope memory",
            "-c",
            "Logical scope test",
            "-l",
            "app.core",
        ])
        .assert()
        .success();
}

#[test]
fn add_with_criticality() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "hazard",
            "-s",
            "Critical memory",
            "-c",
            "High criticality",
            "--criticality",
            "0.95",
        ])
        .assert()
        .success();
}

#[test]
fn add_with_visibility_personal() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "preference",
            "-s",
            "Personal memory",
            "-c",
            "This is personal",
            "--visibility",
            "personal",
        ])
        .assert()
        .success();
}

#[test]
fn add_with_details() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Detailed memory",
            "-c",
            "Content here",
            "--details",
            "Extended details about the decision",
        ])
        .assert()
        .success();
}

#[test]
fn get_by_full_id() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Get test", "Content for get");

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Get test"));
}

#[test]
fn get_with_full_flag() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Full get test", "Full content here");

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id, "--full"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Full get test"));
}

#[test]
fn get_by_prefix() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(
        dir.path(),
        "decision",
        "Prefix get test",
        "Content for prefix",
    );

    // Use first 8 chars as prefix
    let prefix = &id[..8];
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", prefix])
        .assert()
        .success()
        .stdout(predicate::str::contains("Prefix get test"));
}

#[test]
fn get_raw_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Raw get test", "Raw content here");

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id, "--raw"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Raw content here"),
        "raw output should contain content: {}",
        stdout
    );
}

#[test]
fn get_path_returns_md_file() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Path test", "Content for path");

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id, "--path"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().ends_with(".md"),
        "path should end with .md: {}",
        stdout
    );
}

#[test]
fn add_missing_required_fields_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Missing summary and content
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("summary") || stderr.contains("content") || stderr.contains("missing"),
        "error should mention missing required field: {}",
        stderr
    );
}

#[test]
fn add_with_details_file() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Create a details file
    let details_file = dir.path().join("details.txt");
    std::fs::write(
        &details_file,
        "Extended details from file\nWith multiple lines",
    )
    .unwrap();

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Details file test",
            "-c",
            "Content here",
            "--details-file",
            details_file.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_with_confidence() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Confidence test",
            "-c",
            "Content with custom confidence",
            "--confidence",
            "0.95",
        ])
        .assert()
        .success();
}

#[test]
fn get_nonexistent_memory_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "get",
            "nonexistent-id-that-does-not-exist",
        ])
        .assert()
        .failure();
}

#[test]
fn add_with_supersedes() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let old_id = helpers::add_memory(dir.path(), "decision", "Old decision", "Old content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "New decision",
            "-c",
            "Supersedes old one",
            "--supersedes",
            &old_id,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_with_multiple_supersedes() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id1 = helpers::add_memory(dir.path(), "decision", "Old 1", "Old content 1");
    let id2 = helpers::add_memory(dir.path(), "decision", "Old 2", "Old content 2");

    let supersedes_val = format!("{},{}", id1, id2);
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Replaces both",
            "-c",
            "Supersedes two",
            "--supersedes",
            &supersedes_val,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_with_decay_strategy() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    for strategy in &["none", "linear", "exponential", "step"] {
        helpers::cmd()
            .args([
                "--dir",
                dir.path().to_str().unwrap(),
                "add",
                "-t",
                "decision",
                "-s",
                &format!("Decay {} test", strategy),
                "-c",
                "Content with decay",
                "--decay-strategy",
                strategy,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Created memory"));
    }
}

#[test]
fn add_with_all_decay_params() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "context",
            "-s",
            "Full decay config",
            "-c",
            "Memory with full decay settings",
            "--decay-strategy",
            "exponential",
            "--decay-half-life",
            "3600",
            "--decay-ttl",
            "86400",
            "--decay-floor",
            "0.1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_with_decay_floor_validates_range() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // decay-floor > 1.0 should fail validation
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Bad floor",
            "-c",
            "Invalid decay floor",
            "--decay-floor",
            "1.5",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("decay-floor") || stderr.contains("between 0.0 and 1.0"),
        "Expected decay-floor validation error: {}",
        stderr
    );
}

#[test]
fn add_with_supersedes_and_decay() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let old_id = helpers::add_memory(dir.path(), "decision", "Predecessor", "Old content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Successor with decay",
            "-c",
            "Replaces predecessor with decay",
            "--supersedes",
            &old_id,
            "--decay-strategy",
            "linear",
            "--decay-half-life",
            "7200",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created memory"));
}

#[test]
fn add_with_invalid_decay_strategy_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            "Bad strategy",
            "-c",
            "Invalid decay strategy",
            "--decay-strategy",
            "foobar",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("decay") || stderr.contains("Invalid"),
        "Expected decay strategy validation error: {}",
        stderr
    );
}
