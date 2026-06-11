//! Integration tests for `engramdb hook pre-tool-use` / `session-start`.
//!
//! The hook handlers must never load ML models (their queries carry no query
//! text, so retrieval is scope_only). These tests pin that down by running
//! the hooks with an empty `ENGRAMDB_MODEL_CACHE_DIR` and `ENGRAMDB_OFFLINE=1`
//! (see CLAUDE.md "Test isolation"): if a hook ever tried to require an
//! embedding/NLI/reranker/T5 model, it could not load one here, and the
//! output below would change.

use crate::helpers::{add_memory_with_args, cmd, init_store};
use tempfile::TempDir;

/// An `engramdb hook ...` command pointed at `dir`, with model presence
/// pinned to "nothing available": empty model cache + offline.
fn hook_cmd(dir: &std::path::Path, args: &[&str], empty_cache: &TempDir) -> assert_cmd::Command {
    let mut c = cmd();
    c.args(["--dir", dir.to_str().unwrap(), "hook"]);
    c.args(args);
    c.env("ENGRAMDB_MODEL_CACHE_DIR", empty_cache.path());
    c.env("ENGRAMDB_OFFLINE", "1");
    c
}

/// Seed a project with one high-criticality memory scoped to src/main.rs.
/// The file is created on disk so the hook's path canonicalization works.
fn seed_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "").unwrap();

    init_store(project.path());
    add_memory_with_args(
        project.path(),
        &[
            "-t",
            "hazard",
            "-s",
            "Avoid blocking calls in async",
            "-c",
            "Blocking calls in async context cause deadlocks",
            "-p",
            "src/main.rs",
            "--criticality",
            "0.9",
        ],
    );
    project
}

#[test]
fn hook_pre_tool_use_works_with_empty_model_cache_offline() {
    let project = seed_project();
    let empty_cache = TempDir::new().unwrap();

    let input = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": project.path().join("src/main.rs") }
    })
    .to_string();

    let output = hook_cmd(project.path(), &["pre-tool-use"], &empty_cache)
        .write_stdin(input)
        .output()
        .expect("failed to run hook pre-tool-use");

    assert!(
        output.status.success(),
        "hook pre-tool-use must succeed without any models: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("hook output must be valid JSON");
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let ctx = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext must be a string");
    assert!(ctx.contains("[EngramDB]"), "unexpected context: {ctx}");
    assert!(
        ctx.contains("Avoid blocking calls in async"),
        "path-scoped memory must surface without models: {ctx}"
    );
}

#[test]
fn hook_session_start_works_with_empty_model_cache_offline() {
    let project = seed_project();
    let empty_cache = TempDir::new().unwrap();

    let output = hook_cmd(
        project.path(),
        &["session-start", "--min-criticality", "0.7"],
        &empty_cache,
    )
    .output()
    .expect("failed to run hook session-start");

    assert!(
        output.status.success(),
        "hook session-start must succeed without any models: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("hook output must be valid JSON");
    assert_eq!(
        parsed["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let ctx = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext must be a string");
    assert!(
        ctx.contains("Avoid blocking calls in async"),
        "high-criticality memory must surface without models: {ctx}"
    );
    assert!(
        ctx.contains("When you finish the task"),
        "reflection nudge must be present: {ctx}"
    );
}

#[test]
fn hook_pre_tool_use_uninitialized_store_is_silent() {
    let project = TempDir::new().unwrap();
    let empty_cache = TempDir::new().unwrap();

    // No `init` — the hook must exit 0 with no output so a missing store
    // never breaks Claude Code.
    let output = hook_cmd(project.path(), &["pre-tool-use"], &empty_cache)
        .write_stdin(r#"{"tool_name":"Read","tool_input":{"file_path":"/tmp/x.rs"}}"#)
        .output()
        .expect("failed to run hook pre-tool-use");

    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "uninitialized store must produce no output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
