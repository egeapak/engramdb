use super::helpers;
use tempfile::TempDir;

#[test]
fn compress_shows_candidates() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "compress"])
        .assert()
        .success();
}

#[test]
fn compress_with_threshold() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "compress",
            "--threshold",
            "0.99",
        ])
        .assert()
        .success();
}

#[test]
fn compress_empty_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "compress"])
        .assert()
        .success();
}

#[test]
fn compress_with_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Scoped compress",
            "-c",
            "Content",
            "-l",
            "app.core",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "compress",
            "--scope",
            "app.core",
        ])
        .assert()
        .success();
}
