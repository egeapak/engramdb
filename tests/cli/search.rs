use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn search_basic() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // "Rust" should match "Use Rust for backend"
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "search", "Rust"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn search_with_type_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Filtering by decision type should still find the decision memory
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "Rust",
            "-t",
            "decision",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Use Rust"));
}

#[test]
fn search_with_tags_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Searchable tagged",
            "-c",
            "Tagged content for search",
            "--tags",
            "searchable",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "tagged",
            "--tags",
            "searchable",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Searchable tagged"));
}

#[test]
fn search_max_results_limits_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path()); // 3 memories

    // With --json and -n 1, should get exactly 1 result
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "search",
            "Use",
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
    let arr = parsed.as_array().expect("Expected JSON array");
    assert!(
        arr.len() <= 1,
        "Expected at most 1 result with -n 1, got {}",
        arr.len()
    );
}

#[test]
fn search_no_results() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "zzzznonexistentqueryzzzz",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should not contain any seeded memory summaries
    assert!(
        !stdout.contains("Use Rust")
            && !stdout.contains("snake_case")
            && !stdout.contains("Avoid unwrap"),
        "No-match search should not contain seeded memories: {}",
        stdout
    );
}

#[test]
fn search_with_json_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "search",
            "Rust",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Invalid JSON: {} — output: {}", e, stdout);
    });
    // JSON search results should be an array with score and memory fields
    let arr = parsed.as_array().expect("Expected JSON array");
    assert!(!arr.is_empty(), "Search for 'Rust' should find results");
    assert!(
        arr[0].get("score").is_some(),
        "Each result should have a 'score' field"
    );
    assert!(
        arr[0].get("memory").is_some(),
        "Each result should have a 'memory' field"
    );
}

#[test]
fn search_with_physical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Phys search",
            "-c",
            "Physical scope content",
            "-p",
            "src/main.rs",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "Physical",
            "-p",
            "src/main.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Phys search"));
}

#[test]
fn search_with_logical_scope() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "convention",
            "-s",
            "Logic search",
            "-c",
            "Logical scope content",
            "-l",
            "app.core",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "Logical",
            "-l",
            "app.core",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logic search"));
}

#[test]
fn search_with_min_criticality() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::add_memory_with_args(
        dir.path(),
        &[
            "-t",
            "decision",
            "-s",
            "High crit search",
            "-c",
            "High criticality content",
            "--criticality",
            "0.9",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "criticality",
            "--min-criticality",
            "0.5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("High crit search"));
}
