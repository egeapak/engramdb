//! `--help` for every command and subcommand, plus every clap-level failure.
//!
//! These are the outcomes tier 1 structurally cannot reach: clap renders them
//! itself and exits 2 from `Cli::parse()` in `main.rs`, before
//! `engram_cli::run` — and therefore before any `OutputFormatter` — exists.
//!
//! Help text is width-independent here: clap only consults the terminal width
//! under its `wrap_help` feature, which pulls in `terminal_size`, and neither
//! appears in this workspace's dependency graph.

use super::Fixture;

/// One `#[test]` per case rather than a loop, because `assert_snapshot!`
/// panics on the first mismatch — a loop would hide every later case and,
/// worse, would refuse to write their `.snap.new` files on a first run.
macro_rules! help_cases {
    ($($name:ident => [$($arg:expr),* $(,)?]),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let f = Fixture::new();
                insta::assert_snapshot!(stringify!($name), f.run_bare(&[$($arg),*]));
            }
        )*
    };
}

help_cases! {
    // ---- root ----------------------------------------------------------
    help_root_long => ["--help"],
    help_root_short => ["-h"],
    help_version => ["--version"],

    // ---- top-level subcommands (27) -------------------------------------
    help_init => ["init", "--help"],
    help_add => ["add", "--help"],
    help_get => ["get", "--help"],
    help_query => ["query", "--help"],
    help_list => ["list", "--help"],
    help_update => ["update", "--help"],
    help_delete => ["delete", "--help"],
    help_task => ["task", "--help"],
    help_verify => ["verify", "--help"],
    help_config => ["config", "--help"],
    help_stats => ["stats", "--help"],
    help_doctor => ["doctor", "--help"],
    help_projects => ["projects", "--help"],
    help_groups => ["groups", "--help"],
    help_challenge => ["challenge", "--help"],
    help_gc => ["gc", "--help"],
    help_compress => ["compress", "--help"],
    help_serve => ["serve", "--help"],
    help_daemon => ["daemon", "--help"],
    help_completions => ["completions", "--help"],
    help_migrate => ["migrate", "--help"],
    help_rollback => ["rollback", "--help"],
    help_reindex => ["reindex", "--help"],
    help_hook => ["hook", "--help"],
    help_setup => ["setup", "--help"],
    help_review => ["review", "--help"],
    help_harvest => ["harvest", "--help"],

    // ---- task (2) -------------------------------------------------------
    help_task_current => ["task", "current", "--help"],
    help_task_complete => ["task", "complete", "--help"],

    // ---- doctor (2) -----------------------------------------------------
    help_doctor_store => ["doctor", "store", "--help"],
    help_doctor_validate => ["doctor", "validate", "--help"],

    // ---- daemon (4) -----------------------------------------------------
    help_daemon_run => ["daemon", "run", "--help"],
    help_daemon_status => ["daemon", "status", "--help"],
    help_daemon_stop => ["daemon", "stop", "--help"],
    help_daemon_restart => ["daemon", "restart", "--help"],

    // ---- projects (7) ---------------------------------------------------
    help_projects_info => ["projects", "info", "--help"],
    help_projects_list => ["projects", "list", "--help"],
    help_projects_delete => ["projects", "delete", "--help"],
    help_projects_stats => ["projects", "stats", "--help"],
    help_projects_prune => ["projects", "prune", "--help"],
    help_projects_link => ["projects", "link", "--help"],
    help_projects_unlink => ["projects", "unlink", "--help"],

    // ---- groups (5) -----------------------------------------------------
    help_groups_create => ["groups", "create", "--help"],
    help_groups_subscribe => ["groups", "subscribe", "--help"],
    help_groups_unsubscribe => ["groups", "unsubscribe", "--help"],
    help_groups_list => ["groups", "list", "--help"],
    help_groups_members => ["groups", "members", "--help"],

    // ---- hook (6) -------------------------------------------------------
    // No `--help` case for the `external_subcommand` catch-all: clap does not
    // generate help for it. Its behaviour is covered in `snapshot::hook`.
    help_hook_pre_tool_use => ["hook", "pre-tool-use", "--help"],
    help_hook_session_start => ["hook", "session-start", "--help"],
    help_hook_user_prompt_submit => ["hook", "user-prompt-submit", "--help"],
    help_hook_post_tool_use => ["hook", "post-tool-use", "--help"],
    help_hook_session_end => ["hook", "session-end", "--help"],
    help_hook_pre_compact => ["hook", "pre-compact", "--help"],

    // ---- harvest (8) ----------------------------------------------------
    help_harvest_list => ["harvest", "list", "--help"],
    help_harvest_show => ["harvest", "show", "--help"],
    help_harvest_mark => ["harvest", "mark", "--help"],
    help_harvest_index => ["harvest", "index", "--help"],
    help_harvest_search => ["harvest", "search", "--help"],
    help_harvest_summary => ["harvest", "summary", "--help"],
    help_harvest_reset => ["harvest", "reset", "--help"],
    help_harvest_ledger => ["harvest", "ledger", "--help"],

    // ---- harvest ledger (5) ---------------------------------------------
    help_harvest_ledger_list => ["harvest", "ledger", "list", "--help"],
    help_harvest_ledger_show => ["harvest", "ledger", "show", "--help"],
    help_harvest_ledger_export => ["harvest", "ledger", "export", "--help"],
    help_harvest_ledger_rm => ["harvest", "ledger", "rm", "--help"],
    help_harvest_ledger_prune => ["harvest", "ledger", "prune", "--help"],

    // ---- clap-level failures (exit 2) -----------------------------------
    err_unknown_subcommand => ["definitely-not-a-command"],
    err_unknown_flag => ["list", "--not-a-flag"],
    err_missing_required_arg => ["get"],
    err_missing_required_value => ["challenge", "some-id"],
    err_bad_format_value => ["--format", "xml", "list"],
    err_bad_sort_value => ["list", "--sort", "nonsense"],
    err_bad_shell_value => ["completions", "klingon"],
    err_conflicting_format_and_json => ["--format", "pretty", "--json", "list"],
    err_conflicting_content_and_positional =>
        ["add", "-t", "decision", "-s", "s", "-c", "flag-content", "positional-content"],
    err_conflicting_tags_and_tags_add =>
        ["update", "some-id", "--tags", "a", "--tags-add", "b"],
    err_conflicting_reindex_modes => ["reindex", "--embeddings-only", "--index-only"],
    err_superseded_by_requires_invalidate =>
        ["update", "some-id", "--superseded-by", "other-id"],
    err_group_conflicts_with_global =>
        ["add", "-t", "decision", "-s", "s", "-c", "c", "--global", "--group", "g"],
}
