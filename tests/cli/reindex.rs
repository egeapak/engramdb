use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn reindex_full() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Full reindex should rebuild the index
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "reindex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Rebuilt index").or(predicate::str::contains("Reindex")));
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

    // With --no-embeddings init, embeddings-only should succeed (skip embedding)
    // and report either "Embedded" or "Nothing to reindex"
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "reindex",
            "--embeddings-only",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Embedded").or(predicate::str::contains("Nothing to reindex")),
        );
}
