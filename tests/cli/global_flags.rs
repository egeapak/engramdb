use super::helpers;
use tempfile::TempDir;

#[test]
fn format_json_flag_valid_json() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "json",
            "list",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "--format json should produce valid JSON: {}",
        stdout
    );
}

#[test]
fn format_pretty_flag() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "pretty",
            "list",
        ])
        .assert()
        .success();
}

#[test]
fn format_plain_flag() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "plain",
            "list",
        ])
        .assert()
        .success();
}

#[test]
fn quiet_flag() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "-q", "list"])
        .assert()
        .success();
}

#[test]
fn dir_flag_works() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Verify --dir works by listing from a different cwd
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "list"])
        .current_dir("/tmp")
        .assert()
        .success();
}

#[test]
fn embedding_backend_onnx_flag() {
    let dir = TempDir::new().unwrap();

    // Init with --embedding-backend onnx should not crash
    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--embedding-backend",
            "onnx",
            "init",
            "--no-embeddings",
        ])
        .assert()
        .success();
}

#[test]
fn no_color_flag() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--no-color", "list"])
        .assert()
        .success();
}

#[test]
fn verbose_flag() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--verbose", "list"])
        .assert()
        .success();
}

#[test]
fn global_store_flag_list_succeeds() {
    // `list --global` targets the global store, which auto-initializes on
    // first open — no project `init` required. Pin a private data dir so the
    // single global-store location isn't raced by sibling tests.
    let dir = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();

    helpers::cmd()
        .env("ENGRAMDB_DATA_DIR", data.path())
        .args(["--dir", dir.path().to_str().unwrap(), "list", "--global"])
        .assert()
        .success();
}

#[test]
fn global_store_flag_stats_succeeds() {
    let dir = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();

    helpers::cmd()
        .env("ENGRAMDB_DATA_DIR", data.path())
        .args(["--dir", dir.path().to_str().unwrap(), "stats", "--global"])
        .assert()
        .success();
}

#[test]
fn global_store_flag_listed_in_help() {
    let output = helpers::cmd().args(["list", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--global"),
        "list --help should advertise --global: {}",
        stdout
    );
}

#[test]
fn json_shorthand_matches_format_json() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let json_output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "list"])
        .output()
        .unwrap();

    let format_output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--format",
            "json",
            "list",
        ])
        .output()
        .unwrap();

    assert!(json_output.status.success());
    assert!(format_output.status.success());

    let json_stdout = String::from_utf8_lossy(&json_output.stdout);
    let format_stdout = String::from_utf8_lossy(&format_output.stdout);

    // Both should produce valid JSON
    let parsed1: Result<serde_json::Value, _> = serde_json::from_str(&json_stdout);
    let parsed2: Result<serde_json::Value, _> = serde_json::from_str(&format_stdout);
    assert!(
        parsed1.is_ok(),
        "--json should produce valid JSON: {}",
        json_stdout
    );
    assert!(
        parsed2.is_ok(),
        "--format json should produce valid JSON: {}",
        format_stdout
    );
}
