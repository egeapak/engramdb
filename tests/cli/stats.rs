use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn stats_populated_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("3").or(predicate::str::contains("Total")));
}

#[test]
fn stats_json_output_valid() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "stats"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // stats prints JSON followed by embeddings status line — extract just the JSON block
    let json_str = if let Some(end) = stdout.rfind('}') {
        &stdout[..=end]
    } else {
        &stdout
    };
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(json_str);
    assert!(
        parsed.is_ok(),
        "stats --json should contain valid JSON: {}",
        stdout
    );
    let val = parsed.unwrap();
    assert!(val.get("total").is_some(), "JSON should have 'total' key");
}

#[test]
fn stats_empty_store() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "stats"])
        .assert()
        .success();
}

#[test]
fn stats_json_has_expected_keys() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "stats"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // stats prints JSON followed by embeddings status line — extract just the JSON block
    let json_str = if let Some(end) = stdout.rfind('}') {
        &stdout[..=end]
    } else {
        &stdout
    };
    let val: serde_json::Value = serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("Failed to parse stats JSON: {} — output: {}", e, stdout));
    assert!(val.get("total").is_some(), "JSON should have 'total' key");
    assert!(
        val.get("by_type").is_some(),
        "JSON should have 'by_type' key"
    );
    assert!(
        val.get("by_status").is_some(),
        "JSON should have 'by_status' key"
    );
}
