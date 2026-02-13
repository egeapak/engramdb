use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn retrieve_by_path() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Path retrieve test",
            "-c",
            "Content for path",
            "-p",
            "src/main.rs",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Path retrieve test"));
}

#[test]
fn retrieve_by_query() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--query",
            "Rust backend",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn retrieve_with_type_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "decision",
            "-s",
            "Typed retrieve",
            "-c",
            "Decision content",
            "-p",
            "src/main.rs",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "-t",
            "decision",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Typed retrieve"));
}

#[test]
fn retrieve_max_results_limits_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path()); // 3 memories

    // Use JSON to count results
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "retrieve",
            "--query",
            "use",
            "-n",
            "1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Invalid JSON: {} — output: {}", e, stdout);
    });
    let memories = parsed
        .get("memories")
        .and_then(|m| m.as_array())
        .expect("Expected 'memories' array in JSON");
    assert!(
        memories.len() <= 1,
        "Expected at most 1 result with -n 1, got {}",
        memories.len()
    );
}

#[test]
fn retrieve_with_show_scores() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Scored retrieve",
            "-c",
            "Content with scores",
            "-p",
            "src/main.rs",
        ],
    );

    // --show-scores should include score brackets like [0.XX]
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "--show-scores",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('[') && stdout.contains(']'),
        "Expected score brackets in output with --show-scores: {}",
        stdout
    );
}

#[test]
fn retrieve_with_json_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "retrieve",
            "--path",
            "src/main.rs",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Invalid JSON: {} — output: {}", e, stdout);
    });
    assert!(
        parsed.get("memories").is_some(),
        "JSON should have 'memories' key"
    );
    assert!(
        parsed.get("total").is_some(),
        "JSON should have 'total' key"
    );
}

#[test]
fn retrieve_by_logical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Logical retrieve test",
            "-c",
            "Content for logical",
            "-l",
            "db.schema",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "-l",
            "db.schema",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logical retrieve test"));
}

#[test]
fn retrieve_with_detail_level_summary() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Summary detail level should still show results
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--query",
            "Rust",
            "--detail-level",
            "summary",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn retrieve_with_tags_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Tagged retrieve",
            "-c",
            "Tagged content",
            "--tags",
            "findme",
            "-p",
            "src/main.rs",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "--tags",
            "findme",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tagged retrieve"));
}

#[test]
fn retrieve_with_include_expired() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // --include-expired should still return results
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--query",
            "Rust",
            "--include-expired",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn retrieve_with_detail_level_content() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--query",
            "Rust",
            "--detail-level",
            "content",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn retrieve_with_detail_level_full() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--query",
            "Rust",
            "--detail-level",
            "full",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}
