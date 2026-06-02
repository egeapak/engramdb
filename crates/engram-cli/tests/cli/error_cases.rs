use super::helpers;
use tempfile::TempDir;

#[test]
fn list_on_uninit_store_fails() {
    let dir = TempDir::new().unwrap();

    // No init — should fail with an appropriate error
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "list"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("not found")
            || stderr.contains("not initialized")
            || stderr.contains("no such")
            || stderr.contains("does not exist")
            || stderr.contains("error"),
        "error should mention missing store: {}",
        stderr
    );
}

#[test]
fn add_invalid_type_fails_with_message() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "invalid_type_xyz",
            "-s",
            "Test",
            "-c",
            "Content",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("type") || stderr.contains("invalid") || stderr.contains("unknown"),
        "error should mention invalid type: {}",
        stderr
    );
}

#[test]
fn add_invalid_criticality_fails_with_message() {
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
            "Test",
            "-c",
            "Content",
            "--criticality",
            "2.0",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("0.0")
            || stderr.contains("1.0")
            || stderr.contains("range")
            || stderr.contains("between")
            || stderr.contains("criticality"),
        "error should mention valid range: {}",
        stderr
    );
}

#[test]
fn add_with_long_summary_fails_with_message() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let long_summary = "A".repeat(500);

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "add",
            "-t",
            "decision",
            "-s",
            &long_summary,
            "-c",
            "Content for long summary test",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("100") || stderr.contains("summary") || stderr.contains("characters"),
        "error should mention summary length limit: {}",
        stderr
    );
}

#[test]
fn add_negative_criticality_fails() {
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
            "Test",
            "-c",
            "Content",
            "--criticality",
            "-0.5",
        ])
        .assert()
        .failure();
}

#[test]
fn invalid_sort_value_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "list",
            "--sort",
            "invalid",
        ])
        .assert()
        .failure();
}

#[test]
fn invalid_format_value_fails() {
    // clap should reject `--format xml` since OutputFormat only has pretty/json/plain
    helpers::cmd()
        .args(["--format", "xml", "list"])
        .assert()
        .failure();
}

#[test]
fn invalid_visibility_fails() {
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
            "test",
            "-c",
            "content",
            "--visibility",
            "invalid",
        ])
        .assert()
        .failure();
}

#[test]
fn challenge_missing_evidence_fails() {
    // `challenge <id>` without `--evidence` should fail at clap level
    helpers::cmd()
        .args(["challenge", "some-id"])
        .assert()
        .failure();
}

#[test]
fn details_file_nonexistent_fails() {
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
            "test",
            "-c",
            "content",
            "--details-file",
            "/no/such/file/path.txt",
        ])
        .assert()
        .failure();
}
