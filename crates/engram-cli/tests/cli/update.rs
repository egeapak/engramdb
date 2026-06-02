use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn update_summary() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Old summary", "Original content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--summary",
            "New summary",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));

    // Verify the update
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("New summary"));
}

#[test]
fn update_content() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Content update test", "Old content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--content",
            "New content here",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_type() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Type update test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--type",
            "convention",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_criticality() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Criticality test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--criticality",
            "0.99",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_tags_add() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "convention", "Tags add test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--tags-add",
            "new-tag,another-tag",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));

    // Verify tags were added
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id, "--full"])
        .assert()
        .success()
        .stdout(predicate::str::contains("new-tag"));
}

#[test]
fn update_tags_remove() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Tags remove test",
            "-c",
            "Content",
            "--tags",
            "keep-me,remove-me",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--tags-remove",
            "remove-me",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_visibility() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Visibility test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--visibility",
            "personal",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_status() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Status test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--status",
            "needsreview",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_supersedes() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id1 = helpers::add_memory(dir.path(), "decision", "Old decision", "Old content");
    let id2 = helpers::add_memory(dir.path(), "decision", "New decision", "New content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id2,
            "--supersedes",
            &id1,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_nonexistent_memory_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            "nonexistent-id-that-does-not-exist",
            "--summary",
            "New summary",
        ])
        .assert()
        .failure();
}

#[test]
fn update_physical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Phys scope update", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--physical",
            "src/main.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_logical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Logic scope update", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--logical",
            "db.schema",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_confidence() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Confidence update", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--confidence",
            "0.5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_details() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Details update", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--details",
            "Extended info",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_decay_strategy() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Decay strategy test", "Content");

    for strategy in &["none", "linear", "exponential", "step"] {
        helpers::cmd()
            .args([
                "--dir",
                dir.path().to_str().unwrap(),
                "update",
                &id,
                "--decay-strategy",
                strategy,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Updated memory"));
    }
}

#[test]
fn update_decay_half_life() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Decay half-life test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--decay-half-life",
            "3600",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_decay_ttl() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Decay TTL test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--decay-ttl",
            "7200",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_decay_floor() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Decay floor test", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--decay-floor",
            "0.1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_all_decay_params() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Full decay update", "Content");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--decay-strategy",
            "exponential",
            "--decay-half-life",
            "3600",
            "--decay-ttl",
            "86400",
            "--decay-floor",
            "0.05",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated memory"));
}

#[test]
fn update_decay_floor_validates_range() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Bad floor test", "Content");

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--decay-floor",
            "2.0",
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
fn update_invalid_decay_strategy_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Bad strategy test", "Content");

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "update",
            &id,
            "--decay-strategy",
            "invalid_strategy",
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
