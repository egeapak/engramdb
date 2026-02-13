use super::helpers;
use tempfile::TempDir;

#[test]
fn search_basic() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "search", "Rust"])
        .assert()
        .success();
}

#[test]
fn search_with_type_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "test",
            "-t",
            "decision",
        ])
        .assert()
        .success();
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
            "Tagged content",
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
        .success();
}

#[test]
fn search_max_results() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "test",
            "-n",
            "1",
        ])
        .assert()
        .success();
}

#[test]
fn search_no_results() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "search",
            "zzzznonexistentqueryzzzz",
        ])
        .assert()
        .success();
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
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "search --json should produce valid JSON: {}",
        stdout
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
            "scope",
            "-p",
            "src/main.rs",
        ])
        .assert()
        .success();
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
            "scope",
            "-l",
            "app.core",
        ])
        .assert()
        .success();
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
            "High criticality",
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
        .success();
}
