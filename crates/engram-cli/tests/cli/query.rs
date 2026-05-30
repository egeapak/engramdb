use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// rank mode — formerly the `retrieve` subcommand
// ---------------------------------------------------------------------------

#[test]
fn query_rank_by_path() {
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
            "query",
            "--mode",
            "rank",
            "--path",
            "src/main.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Path retrieve test"));
}

#[test]
fn query_rank_by_query_text() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "rank",
            "--query",
            "Rust backend",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn query_rank_with_type_filter() {
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
            "query",
            "--mode",
            "rank",
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
fn query_rank_max_results_limits_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "query",
            "--mode",
            "rank",
            "--query",
            "use",
            "-n",
            "1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — output: {}", e, stdout));
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
fn query_rank_show_scores() {
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

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "rank",
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
fn query_rank_json_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "query",
            "--mode",
            "rank",
            "--path",
            "src/main.rs",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — output: {}", e, stdout));
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
fn query_rank_by_logical_scope() {
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
            "-p",
            "src/db/schema.rs",
            "--criticality",
            "0.9",
        ],
    );

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "rank",
            "-l",
            "db.schema",
            "--path",
            "src/db/schema.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logical retrieve test"));
}

#[test]
fn query_rank_detail_level_summary() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "rank",
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
fn query_rank_with_tags_filter() {
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
            "query",
            "--mode",
            "rank",
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
fn query_rank_with_include_expired() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "rank",
            "--query",
            "Rust",
            "--include-expired",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

// ---------------------------------------------------------------------------
// filter mode — formerly the `search` subcommand
// ---------------------------------------------------------------------------

#[test]
fn query_filter_basic() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "filter",
            "Rust",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Found").or(predicate::str::contains("Use Rust")));
}

#[test]
fn query_filter_with_type_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "filter",
            "Rust",
            "-t",
            "decision",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Use Rust"));
}

#[test]
fn query_filter_with_tags_only() {
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

    // tags alone is a valid filter-mode signal (no --query needed)
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "filter",
            "--tags",
            "searchable",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Searchable tagged"));
}

#[test]
fn query_filter_max_results_limits_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "query",
            "--mode",
            "filter",
            "Use",
            "-n",
            "1",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON: {} — output: {}", e, stdout));
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
fn query_filter_no_results() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "filter",
            "zzzznonexistentqueryzzzz",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Use Rust")
            && !stdout.contains("snake_case")
            && !stdout.contains("Avoid unwrap"),
        "No-match filter should not contain seeded memories: {}",
        stdout
    );
}

#[test]
fn query_filter_with_physical_scope() {
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
            "query",
            "--mode",
            "filter",
            "Physical",
            "-p",
            "src/main.rs",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Phys search"));
}

#[test]
fn query_filter_with_logical_scope() {
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
            "query",
            "--mode",
            "filter",
            "Logical",
            "-l",
            "app.core",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Logic search"));
}

#[test]
fn query_filter_rejects_empty_input() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // mode=filter with only min_criticality (no query/logical/path/tags) must fail.
    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "query",
            "--mode",
            "filter",
            "--min-criticality",
            "0.5",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("filter requires at least one"),
        "Expected validation error, got: {}",
        stderr
    );
}
