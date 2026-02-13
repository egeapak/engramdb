use super::helpers;
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
        .success();
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
        .success();
}

#[test]
fn retrieve_with_type_filter() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

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
        .success();
}

#[test]
fn retrieve_max_results() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "-n",
            "1",
        ])
        .assert()
        .success();
}

#[test]
fn retrieve_with_show_scores() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "--show-scores",
        ])
        .assert()
        .success();
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
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "retrieve --json should produce valid JSON: {}",
        stdout
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
        .success();
}

#[test]
fn retrieve_with_detail_level_summary() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "--detail-level",
            "summary",
        ])
        .assert()
        .success();
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
        .success();
}

#[test]
fn retrieve_with_include_expired() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "retrieve",
            "--path",
            "src/main.rs",
            "--include-expired",
        ])
        .assert()
        .success();
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
            "--path",
            "src/main.rs",
            "--detail-level",
            "content",
        ])
        .assert()
        .success();
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
            "--path",
            "src/main.rs",
            "--detail-level",
            "full",
        ])
        .assert()
        .success();
}
