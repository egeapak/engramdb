use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn default_init_creates_store() {
    let dir = TempDir::new().unwrap();
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "init",
            "--no-embeddings",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized EngramDB store"));

    assert!(dir.path().join(".engramdb").exists());
    assert!(dir.path().join(".engramdb/config.toml").exists());
    assert!(dir.path().join(".engramdb/manifest.toml").exists());
}

#[test]
fn init_no_embeddings_skips_model() {
    let dir = TempDir::new().unwrap();
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "init",
            "--no-embeddings",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Initialized"));

    // Should not contain any model download messages
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "init",
            "--no-embeddings",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Downloading") && !stdout.contains("downloading"),
        "should not download models with --no-embeddings"
    );
}

#[test]
fn double_init_succeeds_or_warns() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Second init - should either succeed with warning or fail gracefully
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "init",
            "--no-embeddings",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr).to_lowercase();

    // Accept: exits 0 (idempotent), or exits non-zero, or warns
    assert!(
        output.status.success()
            || combined.contains("already")
            || combined.contains("initialized")
            || combined.contains("exists"),
        "expected success, warning, or error about existing store. stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}
