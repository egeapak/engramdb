use super::helpers;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn projects_info() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // Project info should contain the word "project" and show path/id
    helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("project"));
}

#[test]
fn projects_list() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    // After init, project list should show at least the current project path
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "list"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The list groups projects into a directory tree; the current project
    // appears at least by its basename (under a folder header or inline).
    let basename = dir.path().file_name().unwrap().to_str().unwrap();
    assert!(
        stdout.contains(basename) || stdout.contains("project"),
        "Projects list should reference the project: {}",
        stdout
    );
}

#[test]
fn projects_stats() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());
    helpers::seed_store(dir.path());

    // Stats should show counts
    let output = helpers::cmd()
        .args(["--dir", dir.path().to_str().unwrap(), "projects", "stats"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("total") || stdout.contains("3") || stdout.contains("memor"),
        "Stats should show memory counts: {}",
        stdout
    );
}

#[test]
fn projects_info_json_output() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    let output = helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "--json",
            "projects",
            "info",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse projects info JSON: {} — output: {}",
            e, stdout
        )
    });
    assert!(
        val.get("project_id").is_some(),
        "JSON should have 'project_id' key: {}",
        stdout
    );
}

#[test]
fn projects_delete_nonexistent_fails() {
    let dir = TempDir::new().unwrap();
    helpers::init_store(dir.path());

    helpers::cmd()
        .args([
            "--dir",
            dir.path().to_str().unwrap(),
            "projects",
            "delete",
            "fake-id",
            "--force",
        ])
        .assert()
        .failure();
}

/// A directory carrying `.engramdb/memories/` that this machine's registry has
/// never seen — the case `projects discover` exists for (a fresh clone of a
/// repo whose memories are committed, a restored backup, a lost registry).
fn unregistered_project(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join(".engramdb").join("memories")).unwrap();
    dir
}

#[test]
fn projects_discover_dry_run_reports_candidates_without_registering() {
    let tree = TempDir::new().unwrap();
    unregistered_project(tree.path(), "found-me");

    let output = helpers::cmd()
        .args([
            "--json",
            "projects",
            "discover",
            tree.path().to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("discover --dry-run JSON: {e} — output: {stdout}"));
    assert_eq!(val["dry_run"], serde_json::Value::Bool(true));
    let candidates = val["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{stdout}");
    assert!(candidates[0]["path"]
        .as_str()
        .unwrap()
        .ends_with("found-me"));

    // Nothing was registered.
    let listed = helpers::cmd()
        .args(["--json", "projects", "list"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("found-me"));
}

#[test]
fn projects_discover_yes_registers_found_projects() {
    let tree = TempDir::new().unwrap();
    unregistered_project(tree.path(), "adopt-me");

    let output = helpers::cmd()
        .args([
            "--json",
            "projects",
            "discover",
            tree.path().to_str().unwrap(),
            "--yes",
            // Registration is what's under test; skipping the rebuild keeps
            // this off the model-loading path.
            "--no-index",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("discover JSON: {e} — output: {stdout}"));
    assert_eq!(val["registered"].as_array().unwrap().len(), 1, "{stdout}");

    // Every key the CLI reference documents for a real run, so a rename can't
    // break the published contract while CI stays green.
    for key in [
        "root",
        "scanned_dirs",
        "depth_limited",
        "unreadable_dirs",
        "dry_run",
        "no_index",
        "found_unregistered",
        "skipped",
        "registered",
        "declined",
        "errors",
    ] {
        assert!(val.get(key).is_some(), "missing key {key} in {stdout}");
    }
    assert_eq!(val["no_index"], serde_json::Value::Bool(true));
    // `--no-index` must report "not rebuilt", not "rebuilt and found nothing".
    assert_eq!(val["registered"][0]["indexed"], serde_json::Value::Null);
    assert_eq!(val["registered"][0]["embedded"], serde_json::Value::Null);

    let listed = helpers::cmd()
        .args(["--json", "projects", "list"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("adopt-me"),
        "the adopted project must show up in the registry listing"
    );
}

#[test]
fn projects_discover_json_without_yes_refuses_to_prompt() {
    let tree = TempDir::new().unwrap();
    unregistered_project(tree.path(), "needs-consent");

    helpers::cmd()
        .args([
            "--json",
            "projects",
            "discover",
            tree.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--yes"));
}

#[test]
fn projects_discover_reports_skipped_worktrees_in_json() {
    // A linked worktree carrying a committed `.engramdb/` must be reported as
    // skipped rather than adopted as an independent root project — and must be
    // visible in JSON, where the human warning is suppressed.
    let tree = TempDir::new().unwrap();
    let main = tree.path().join("main");
    let wt = tree.path().join("feature");
    let wt_gitdir = main.join(".git").join("worktrees").join("feature");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    std::fs::create_dir_all(&wt_gitdir).unwrap();
    std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
    unregistered_project(tree.path(), "feature");
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", wt_gitdir.display()),
    )
    .unwrap();

    let output = helpers::cmd()
        .args([
            "--json",
            "projects",
            "discover",
            tree.path().to_str().unwrap(),
            "--yes",
            "--no-index",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("discover JSON: {e} — output: {stdout}"));
    assert!(
        val["registered"].as_array().unwrap().is_empty(),
        "a worktree must not be registered as its own project: {stdout}"
    );
    let skipped = val["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{stdout}");
    assert_eq!(skipped[0]["reason"], "git_worktree");
}
