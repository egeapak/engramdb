//! The Claude Code hook handlers.
//!
//! Every case here must exit 0. Hooks fire on every Read/Write/Edit in a
//! session, so a non-zero exit surfaces as an error on each one; `run`
//! therefore wraps hook dispatch in a fail-open backstop that logs and returns
//! `Ok(())`, and an unrecognized event name is reported on stderr rather than
//! being treated as a clap failure. Garbage on stdin is the case that matters
//! most — it is the one a version-skewed Claude Code actually produces.

use super::Fixture;

/// A hook event body for the tool-use hooks.
fn pre_tool_use_event(path: &str) -> String {
    serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": path }
    })
    .to_string()
}

/// A project with one high-criticality memory scoped to `src/main.rs`, which
/// exists on disk so the hook's path canonicalization resolves.
fn seeded(f: &Fixture) {
    let src = f.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
    f.init();
    f.run(&[
        "add",
        "-t",
        "hazard",
        "-s",
        "Avoid blocking calls in async",
        "-c",
        "Blocking calls in an async context cause deadlocks.",
        "-p",
        "src/main.rs",
        "--criticality",
        "0.9",
    ]);
}

/// `(test name, hook subcommand, stdin)` for the four stdin shapes every hook
/// has to survive.
macro_rules! hook_cases {
    ($($name:ident => ($sub:expr, $stdin:expr)),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let f = Fixture::new();
                seeded(&f);
                let stdin: String = $stdin(&f);
                insta::assert_snapshot!(
                    stringify!($name),
                    f.run_with_stdin(&["hook", $sub], &stdin)
                );
            }
        )*
    };
}

fn valid_tool_event(f: &Fixture) -> String {
    pre_tool_use_event(f.path().join("src/main.rs").to_str().unwrap())
}
fn empty(_: &Fixture) -> String {
    String::new()
}
fn garbage(_: &Fixture) -> String {
    "not json at all {{{".to_string()
}
fn empty_json(_: &Fixture) -> String {
    "{}".to_string()
}

hook_cases! {
    // ---- pre-tool-use ---------------------------------------------------
    hook_pre_tool_use_valid => ("pre-tool-use", valid_tool_event),
    hook_pre_tool_use_empty_stdin => ("pre-tool-use", empty),
    hook_pre_tool_use_garbage_stdin => ("pre-tool-use", garbage),
    hook_pre_tool_use_empty_json => ("pre-tool-use", empty_json),

    // ---- session-start --------------------------------------------------
    hook_session_start_empty_json => ("session-start", empty_json),
    hook_session_start_empty_stdin => ("session-start", empty),
    hook_session_start_garbage_stdin => ("session-start", garbage),

    // ---- user-prompt-submit ---------------------------------------------
    hook_user_prompt_submit_empty_json => ("user-prompt-submit", empty_json),
    hook_user_prompt_submit_garbage_stdin => ("user-prompt-submit", garbage),

    // ---- post-tool-use --------------------------------------------------
    hook_post_tool_use_valid => ("post-tool-use", valid_tool_event),
    hook_post_tool_use_garbage_stdin => ("post-tool-use", garbage),

    // ---- session-end / pre-compact --------------------------------------
    hook_session_end_empty_json => ("session-end", empty_json),
    hook_session_end_garbage_stdin => ("session-end", garbage),
    hook_pre_compact_empty_json => ("pre-compact", empty_json),
    hook_pre_compact_garbage_stdin => ("pre-compact", garbage),
}

/// `session-start` takes the only hook flag, and it gates what gets injected.
#[test]
fn hook_session_start_min_criticality_filters() {
    let f = Fixture::new();
    seeded(&f);
    insta::assert_snapshot!(
        "hook_session_start_min_criticality_high",
        f.run_with_stdin(
            &["hook", "session-start", "--min-criticality", "0.99"],
            "{}"
        )
    );
}

#[test]
fn hook_session_start_min_criticality_includes() {
    let f = Fixture::new();
    seeded(&f);
    insta::assert_snapshot!(
        "hook_session_start_min_criticality_low",
        f.run_with_stdin(&["hook", "session-start", "--min-criticality", "0.1"], "{}")
    );
}

/// Version skew: Claude Code names an event this binary predates. It must warn
/// on stderr and still exit 0, not exit 2 the way clap would for an unknown
/// subcommand anywhere else.
#[test]
fn hook_unknown_event_name_exits_zero() {
    let f = Fixture::new();
    seeded(&f);
    insta::assert_snapshot!(
        "hook_unknown_event_name",
        f.run_with_stdin(&["hook", "some-future-event"], "{}")
    );
}

/// A bare `engramdb hook` with no subcommand takes the same fail-open path.
#[test]
fn hook_with_no_subcommand_exits_zero() {
    let f = Fixture::new();
    seeded(&f);
    insta::assert_snapshot!("hook_no_subcommand", f.run_with_stdin(&["hook"], "{}"));
}

/// Hooks run on a store that was never initialized too — a session can start
/// in any directory. That must still be silent and exit 0.
#[test]
fn hook_on_uninitialized_store_exits_zero() {
    let f = Fixture::new();
    f.write_config_only();
    insta::assert_snapshot!(
        "hook_uninitialized_store",
        f.run_with_stdin(&["hook", "session-start"], "{}")
    );
}
