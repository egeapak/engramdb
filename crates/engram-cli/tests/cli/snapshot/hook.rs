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

// ---- hook call stats --------------------------------------------------------
//
// Durations differ on every run, so these assert on structure and counts
// rather than snapshotting the output.

fn hook_log(f: &Fixture) -> Vec<serde_json::Value> {
    let path = f.path().join(".engramdb/state/hook_stats.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("every hook log line is JSON"))
        .collect()
}

/// Each known hook appends one line; `stats --json` reports per-hook counts and
/// the p50 / p99 fields.
#[test]
fn hook_calls_are_recorded_and_reported_by_stats() {
    let f = Fixture::new();
    seeded(&f);
    let event = valid_tool_event(&f);
    f.run_with_stdin(&["hook", "pre-tool-use"], &event);
    f.run_with_stdin(&["hook", "pre-tool-use"], "not json at all {{{");
    f.run_with_stdin(&["hook", "pre-compact"], "{}");
    // Unknown names are never recorded: the name comes from a config this
    // binary does not understand.
    f.run_with_stdin(&["hook", "some-future-event"], "{}");

    let log = hook_log(&f);
    let names: Vec<&str> = log.iter().map(|r| r["hook"].as_str().unwrap()).collect();
    assert_eq!(names, ["pre-tool-use", "pre-tool-use", "pre-compact"]);
    for r in &log {
        assert!(r["ms"].as_f64().unwrap() >= 0.0, "{r}");
        assert!(r["ts"].is_string(), "{r}");
        assert!(r["ok"].is_boolean(), "{r}");
        assert!(r["id"].is_string(), "{r}");
        assert_eq!(r["path"], "in_process", "{r}");
    }

    let out = f.run(&["stats", "--json"]);
    let start = out.find("--- stdout ---\n").unwrap() + "--- stdout ---\n".len();
    let end = out.find("--- stderr ---").unwrap();
    let json: serde_json::Value = serde_json::from_str(&out[start..end]).expect(&out);
    let hooks = &json["hooks"]["by_hook"];
    assert_eq!(hooks["pre-tool-use"]["calls"], 2, "{json}");
    assert_eq!(hooks["pre-compact"]["calls"], 1, "{json}");
    for key in ["errors", "avg", "p50", "p95", "p99"] {
        assert!(hooks["pre-tool-use"].get(key).is_some(), "missing {key}");
    }
    assert!(hooks.get("some-future-event").is_none());
    assert_eq!(hooks["pre-tool-use"]["by_path"]["in_process"]["calls"], 2);
    assert!(hooks["pre-tool-use"]["by_path"].get("daemon").is_none());
    assert!(json["hooks"]["since"].is_string());

    let pretty = f.run(&["--format", "pretty", "stats"]);
    assert!(pretty.contains("Hook calls (since "), "{pretty}");
    // The fixture forces in-process hooks (`ENGRAMDB_IN_PROCESS`), so the
    // single path is named on the hook's own line.
    assert!(
        pretty.contains("  pre-tool-use (in-process): 2 calls, p50 "),
        "{pretty}"
    );
}

/// A store with no hook history shows no hook section at all.
#[test]
fn stats_without_hook_calls_has_no_hooks_section() {
    let f = Fixture::new();
    seeded(&f);
    let out = f.run(&["stats", "--json"]);
    assert!(!out.contains("\"hooks\""), "{out}");
}

/// `[stats].enabled = false` stops hooks from recording.
#[test]
fn hook_calls_are_not_recorded_when_stats_disabled() {
    let f = Fixture::new();
    seeded(&f);
    let config_path = f.path().join(".engramdb/config.toml");
    let mut config: toml::Table =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    config
        .entry("stats")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
        .unwrap()
        .insert("enabled".into(), toml::Value::Boolean(false));
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();

    f.run_with_stdin(&["hook", "pre-compact"], "{}");
    assert!(hook_log(&f).is_empty());
}

/// A directory with only a config (never `init`ed) collects no hook state.
#[test]
fn hook_calls_are_not_recorded_on_uninitialized_store() {
    let f = Fixture::new();
    f.write_config_only();
    f.run_with_stdin(&["hook", "pre-compact"], "{}");
    assert!(!f.path().join(".engramdb/state/hook_stats.jsonl").exists());
}
