use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn reindex_full() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "reindex"])
        .assert()
        .success();
}

#[test]
fn reindex_index_only() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "reindex",
            "--index-only",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rebuilt index"));
}

#[test]
fn reindex_embeddings_only() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // With --no-embeddings init, this should still succeed (just skip embedding)
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "reindex",
            "--embeddings-only",
        ])
        .assert()
        .success();
}
