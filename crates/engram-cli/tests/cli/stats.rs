use super::helpers;
use predicates::prelude::*;
use std::fs;
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

// =====================================================================
// `print_embeddings_status` branch coverage (src/cli/commands/stats.rs).
//
// The status line at the bottom of `engramdb stats` runs through
// `print_embeddings_status`, which dispatches on the configured provider
// name and backend (CRAP 157, was ~30% covered). We drive its branches
// by writing different `[embeddings]` sections into the project config
// and asserting on the rendered status line.
// =====================================================================

/// Overwrite the project's config.toml with the given `[embeddings]` body.
fn write_embeddings_config(dir: &std::path::Path, body: &str) {
    let config_path = dir.join(".engramdb").join("config.toml");
    fs::write(&config_path, body).expect("failed to write config.toml");
}

#[test]
fn stats_embeddings_status_unknown_provider() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    write_embeddings_config(
        dir.path(),
        "[embeddings]\nprovider = \"bogus-provider-xyz\"\ndimensions = 384\nmax_tokens = 256\n",
    );

    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "stats"])
        .assert()
        .success()
        // Drives the `other => { ... return; }` early-return arm.
        .stdout(predicate::str::contains(
            "Not available (unknown provider 'bogus-provider-xyz')",
        ));
}

#[test]
fn stats_embeddings_status_onnx_backend_model_missing() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    // Point the model cache at an empty temp dir AND force offline mode so nomic
    // is *guaranteed* unstaged regardless of what the developer has cached
    // locally (an empty cache alone isn't enough — fastembed would just download
    // it). Forcing the ONNX backend disables any Ollama fallback → the "run
    // 'engramdb init'" branch (backend == Onnx, not available).
    let empty_cache = TempDir::new().unwrap();
    write_embeddings_config(
        dir.path(),
        "[embeddings]\nprovider = \"nomic-embed-text\"\ndimensions = 768\nmax_tokens = 8192\n",
    );

    helpers::cmd()
        .env("ENGRAMDB_MODEL_CACHE_DIR", empty_cache.path())
        .env("ENGRAMDB_OFFLINE", "1")
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--embedding-backend",
            "onnx",
            "stats",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Not available (run 'engramdb init' to download model)",
        ));
}

#[test]
fn stats_embeddings_status_all_minilm_available_via_onnx() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    // The default all-MiniLM model is pre-staged in the test cache, so the
    // ONNX-available branch ("Available (... via ONNX)") fires.
    write_embeddings_config(dir.path(), "[embeddings]\nprovider = \"all-minilm\"\n");

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--embedding-backend",
            "onnx",
            "stats",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available (all-minilm via ONNX)"));
}

// Finding #7: `stats --json` must emit a single valid JSON document on stdout
// (previously the embeddings-status / health lines were printed raw after the
// JSON object, corrupting it for scripted consumers).
#[test]
fn stats_json_is_valid_single_document() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "--json", "stats"])
        .output()
        .unwrap();
    assert!(output.status.success());
    // Parses as exactly one JSON value (trailing raw text would fail this).
    let _: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stats --json must be valid JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
}

// Finding #7/#8: `stats --daemon --json` with no daemon running must still emit
// valid JSON (the persisted-snapshot fallback path), not abort or print raw text.
#[test]
fn stats_daemon_json_is_valid_when_not_running() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "stats",
            "--daemon",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let _: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stats --daemon --json must be valid JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
}
