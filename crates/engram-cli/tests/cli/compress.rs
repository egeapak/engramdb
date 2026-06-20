use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn compress_shows_candidates() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Non-TTY defaults to JSON; compress emits a single structured object with a
    // `candidates` array (finding #7 — no raw text mixed into the JSON stream).
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "compress"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("compress JSON output must be a single valid JSON document");
    assert!(
        v["candidates"].is_array(),
        "expected a candidates array: {v}"
    );
}

#[test]
fn compress_with_threshold() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Very high threshold (0.99) should make most memories candidates
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "compress",
            "--threshold",
            "0.99",
        ])
        .assert()
        .success()
        // Non-TTY defaults to JSON; output must be a single valid JSON
        // document (finding #7), not human text mixed into the stream.
        .stdout(predicate::function(|out: &[u8]| {
            serde_json::from_slice::<serde_json::Value>(out).is_ok()
        }));
}

#[test]
fn compress_empty_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Empty store → valid JSON with an empty candidates array (finding #7).
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "compress"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let v: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("compress JSON output must be a single valid JSON document");
    assert_eq!(
        v["candidates"].as_array().map(|a| a.len()),
        Some(0),
        "empty store must yield zero candidates: {v}"
    );
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
        .success()
        // Non-TTY defaults to JSON; output must be a single valid JSON
        // document (finding #7), not human text mixed into the stream.
        .stdout(predicate::function(|out: &[u8]| {
            serde_json::from_slice::<serde_json::Value>(out).is_ok()
        }));
}
