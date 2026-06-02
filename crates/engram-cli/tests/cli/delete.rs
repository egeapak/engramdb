use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn delete_with_force() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    let id = helpers::add_memory(dir.path(), "decision", "Delete me", "Content to delete");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "delete",
            &id,
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deleted memory"));

    // Verify it's gone
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "get", &id])
        .assert()
        .failure();
}

#[test]
fn delete_nonexistent_memory_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "delete",
            "nonexistent-id-that-does-not-exist",
            "--force",
        ])
        .assert()
        .failure();
}
