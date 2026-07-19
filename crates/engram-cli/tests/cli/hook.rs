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

/// §14.15 process-level robustness: every hook subcommand must exit 0 on
/// empty or garbage stdin — a hook exiting non-zero on malformed input
/// breaks the host Claude Code session, the exact contract these handlers
/// exist to protect. (session-start emits its reflection nudge even on
/// malformed stdin — by design — so only exit codes are asserted here;
/// silence is asserted separately for the handlers that require input.)
#[test]
fn hook_all_subcommands_malformed_stdin_exit_zero() {
    let project = seed_project();
    let empty_cache = TempDir::new().unwrap();

    let subcommands = [
        "pre-tool-use",
        "session-start",
        "user-prompt-submit",
        "post-tool-use",
        "session-end",
        "pre-compact",
    ];
    for sub in subcommands {
        for stdin in ["", "not json at all {{{", "42", "null"] {
            let output = hook_cmd(project.path(), &[sub], &empty_cache)
                .write_stdin(stdin)
                .output()
                .unwrap_or_else(|e| panic!("failed to run hook {sub}: {e}"));
            assert!(
                output.status.success(),
                "hook {sub} must exit 0 on stdin {stdin:?}; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

/// The event-driven handlers (everything except session-start and
/// pre-compact, which emit static context regardless) must stay SILENT on
/// malformed stdin — garbage in, no additionalContext out.
#[test]
fn hook_event_handlers_malformed_stdin_are_silent() {
    let project = seed_project();
    let empty_cache = TempDir::new().unwrap();

    for sub in [
        "pre-tool-use",
        "user-prompt-submit",
        "post-tool-use",
        "session-end",
    ] {
        for stdin in ["", "not json at all {{{"] {
            let output = hook_cmd(project.path(), &[sub], &empty_cache)
                .write_stdin(stdin)
                .output()
                .unwrap_or_else(|e| panic!("failed to run hook {sub}: {e}"));
            assert!(
                output.stdout.is_empty(),
                "hook {sub} must emit nothing on stdin {stdin:?}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}

/// SessionEnd housekeeping end-to-end: the session→task mapping is cleared,
/// and with `[epistemic] demote_on_session_end = true` the ended task's
/// task-scoped memories are demoted to the 14-day curve (§11.2 via §8.5.3).
#[test]
fn hook_session_end_clears_mapping_and_demotes_when_configured() {
    let project = seed_project();
    let empty_cache = TempDir::new().unwrap();

    let task_mem = add_memory_with_args(
        project.path(),
        &[
            "-t",
            "decision",
            "-s",
            "Pin rc12 for this task",
            "-c",
            "task-scoped decision body",
            "--origin-task",
            "feat-x",
            "--generality",
            "task",
        ],
    );

    // Opt in to demotion-on-session-end.
    std::fs::write(
        project.path().join(".engramdb").join("config.toml"),
        "[epistemic]\ndemote_on_session_end = true\n",
    )
    .unwrap();

    // Declare the mapping the way a session would.
    let out = cmd()
        .args([
            "--dir",
            project.path().to_str().unwrap(),
            "task",
            "current",
            "feat-x",
            "--session-id",
            "sess-end-e2e",
        ])
        .output()
        .expect("task current failed");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output = hook_cmd(project.path(), &["session-end"], &empty_cache)
        .write_stdin(r#"{"session_id":"sess-end-e2e"}"#)
        .output()
        .expect("failed to run hook session-end");
    assert!(output.status.success());

    // Mapping cleared.
    let mapping = std::fs::read_to_string(
        project
            .path()
            .join(".engramdb")
            .join("state")
            .join("session_tasks.json"),
    )
    .unwrap_or_default();
    assert!(
        !mapping.contains("sess-end-e2e"),
        "session mapping must be cleared: {mapping}"
    );

    // Task-scoped memory demoted to the 14d exponential curve.
    let get = cmd()
        .args([
            "--dir",
            project.path().to_str().unwrap(),
            "get",
            &task_mem,
            "--format",
            "json",
        ])
        .output()
        .expect("get failed");
    assert!(get.status.success());
    let mem: serde_json::Value = serde_json::from_slice(&get.stdout).unwrap();
    let decay = &mem["decay"];
    assert_eq!(
        decay["strategy"].as_str(),
        Some("exponential"),
        "expected demotion curve, got: {decay}"
    );
}
