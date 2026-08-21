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

    // `--embeddings-only` must succeed and report a definite outcome. Which of
    // the three it is depends on the machine, so all three are accepted:
    //
    // - "Embedded N"    — a provider resolved and the vectors were (re)built.
    // - "Reused N"      — a provider resolved and the vectors were already
    //                     current, so the digest check skipped them. This one
    //                     only became reachable when reindex stopped being
    //                     unconditionally from-scratch.
    // - "Nothing to reindex" — no provider, no vectors, nothing to say.
    //
    // What must never happen is silence: every path reports, and the
    // `Nothing to reindex` guard accounts for skips so a fully-skipped run is
    // not misreported as an empty one.
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
            predicate::str::contains("Embedded")
                .or(predicate::str::contains("Reused"))
                .or(predicate::str::contains("Nothing to reindex")),
        );
}
