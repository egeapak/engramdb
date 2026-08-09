//! The environment-facing commands: `doctor`, `setup`, `daemon`,
//! `completions`, `serve`, `review`.
//!
//! These are the ones the harness works hardest to pin. `doctor` in particular
//! reports the machine back at you — the binary on `$PATH`, the registry, disk
//! usage, `~/.claude` plugin state, the daemon log — so it is only snapshottable
//! because `Fixture` redirects `HOME`, `XDG_*`, the registry and `PATH`, and
//! because the fixture config disables the model-backed subsystems.
//!
//! `serve`, `daemon run` and `daemon restart` are absent by design: they start
//! a long-running process. Their `--help` is in `snapshot::help`, and the fast
//! failure path is below.

use super::Fixture;

// =====================================================================
// doctor — renderer-thin, all three formats
// =====================================================================

#[test]
fn doctor_environment() {
    let f = Fixture::new();
    f.init();
    snap_all_formats!(f, "doctor_environment", &["doctor"]);
}

#[test]
fn doctor_on_uninitialized_store() {
    let f = Fixture::new();
    f.write_config_only();
    insta::assert_snapshot!("doctor_uninitialized", f.run(&["doctor"]));
}

#[test]
fn doctor_store() {
    let f = Fixture::new();
    f.init();
    f.seed();
    snap_all_formats!(f, "doctor_store", &["doctor", "store"]);
}

/// `doctor validate` loads each configured model and runs a test inference.
/// With rerank/NLI off, the keyword titler, an empty model cache and
/// `ENGRAMDB_OFFLINE`, nothing is loadable — which is the deterministic branch
/// and the one a fresh install hits.
#[test]
fn doctor_validate() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("doctor_validate", f.run(&["doctor", "validate"]));
}

#[test]
fn doctor_global() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("doctor_global", f.run(&["doctor", "--global"]));
}

/// `--fix` without a terminal lists what it *would* do and stops.
///
/// `run_environment_check` gates the prompt on
/// `std::io::stdout().is_terminal()` read directly — not on the formatter, not
/// on the prompter — so under a pipe this is the branch that runs, and it is
/// the one a CI job or a scripted invocation actually sees. The prompted
/// variant is unreachable from any test for the same reason: nothing injectable
/// stands between the code and the real stdout.
#[test]
fn doctor_fix_without_tty_lists_actions() {
    let f = Fixture::new();
    f.write_config_only();
    insta::assert_snapshot!("doctor_fix_no_tty", f.run(&["doctor", "--fix"]));
}

/// `--fix --yes` skips the terminal check entirely and applies every action.
///
/// On an initialised store the applied actions are the reindex and the
/// embedding-model check, both of which are no-ops here — so this pins that
/// auto-repair on a healthy store is quiet and exits 0.
///
#[test]
fn doctor_fix_yes_on_healthy_store() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("doctor_fix_yes", f.run(&["doctor", "--fix", "--yes"]));
}

/// The same on an *unregistered* store — the case that made `--fix`
/// destructive.
///
/// `doctor` warns "Registry: not registered", `--fix` answers that by running
/// `projects prune`, and the sweep used to delete the project's own data
/// directory because nothing in the registry pointed at it. The personal
/// memories under `projects/<id>/personal/` are the only copy, so they went
/// with it, and the run then exited 1 on a bare `IO error: No such file or
/// directory` when the next fix action reached for the deleted directory.
/// `prune_stale_projects` now spares the project it is running in, so the
/// sweep reports `orphans_removed: 0` and the store is still readable
/// afterwards — the whole loop, end to end through the binary.
#[test]
fn doctor_fix_yes_on_unregistered_store() {
    let f = Fixture::new();
    f.init();
    // A *personal* memory is the stake. Shared memories live in the project
    // tree and a reindex rebuilds them; this one exists only inside the data
    // directory the sweep used to delete.
    f.seed_personal("Personal note", "Only copy lives in the global data dir");
    f.deregister();
    insta::assert_snapshot!(
        "doctor_fix_yes_unregistered",
        f.run(&["doctor", "--fix", "--yes"])
    );
    // The point of the fix: it is still there to be read afterwards.
    insta::assert_snapshot!("doctor_fix_yes_unregistered_after", f.run(&["list"]));
}

// =====================================================================
// daemon — renderer-thin; only the no-daemon branch is reachable
// =====================================================================

#[test]
fn daemon_status_not_running() {
    let f = Fixture::new();
    f.init();
    snap_all_formats!(f, "daemon_status_not_running", &["daemon", "status"]);
}

#[test]
fn daemon_stop_not_running() {
    let f = Fixture::new();
    f.init();
    snap_all_formats!(f, "daemon_stop_not_running", &["daemon", "stop"]);
}

// =====================================================================
// completions
// =====================================================================

#[test]
fn completions_per_shell() {
    let f = Fixture::new();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        insta::assert_snapshot!(
            format!("completions_{shell}"),
            f.run_bare(&["completions", shell])
        );
    }
}

// =====================================================================
// setup — always --dry-run, always against a temp --claude-dir
// =====================================================================

#[test]
fn setup_dry_run_project_scope() {
    let f = Fixture::new();
    f.init();
    let claude = f.path().join("claude-home");
    std::fs::create_dir_all(&claude).unwrap();
    insta::assert_snapshot!(
        "setup_dry_run_project",
        f.run(&[
            "setup",
            "--dry-run",
            "--claude-dir",
            claude.to_str().unwrap()
        ])
    );
}

#[test]
fn setup_dry_run_global_scope() {
    let f = Fixture::new();
    f.init();
    let claude = f.path().join("claude-home");
    std::fs::create_dir_all(&claude).unwrap();
    insta::assert_snapshot!(
        "setup_dry_run_global",
        f.run(&[
            "setup",
            "--dry-run",
            "--global",
            "--claude-dir",
            claude.to_str().unwrap()
        ])
    );
}

/// The plugin branch, with a stub CLI standing in for a real one.
///
/// `--global` because that is the only scope that probes: project scope writes
/// `.mcp.json` and never asks about a plugin, which is why running it with a
/// stub on PATH produces bytes identical to running it without one.
///
/// Every other `setup` case runs with `claude` stripped from `PATH`, so they
/// all take the "not found, falling back to settings.json" route. That was not
/// a choice until now — it was whatever the machine happened to have installed,
/// and `setup_dry_run_global` duly passed on a developer box with the CLI and
/// failed on CI without it. Both branches are pinned now, neither by accident.
#[cfg(unix)]
#[test]
fn setup_dry_run_with_claude_cli() {
    let f = Fixture::new();
    f.init();
    let claude = f.path().join("claude-home");
    std::fs::create_dir_all(&claude).unwrap();
    insta::assert_snapshot!(
        "setup_dry_run_with_claude_cli",
        f.run_with_claude_cli(&[
            "setup",
            "--dry-run",
            "--global",
            "--claude-dir",
            claude.to_str().unwrap()
        ])
    );
}

#[test]
fn setup_dry_run_no_plugin() {
    let f = Fixture::new();
    f.init();
    let claude = f.path().join("claude-home");
    std::fs::create_dir_all(&claude).unwrap();
    insta::assert_snapshot!(
        "setup_dry_run_no_plugin",
        f.run(&[
            "setup",
            "--dry-run",
            "--no-plugin",
            "--claude-dir",
            claude.to_str().unwrap()
        ])
    );
}

// =====================================================================
// serve — the one outcome that does not start a server
// =====================================================================

#[test]
fn serve_unknown_transport_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "serve_unknown_transport_fails",
        f.run(&["serve", "--transport", "carrier-pigeon"])
    );
}

// =====================================================================
// review — the non-interactive outcomes
// =====================================================================

/// Nothing challenged and nothing stale: `review` reports and exits without
/// ever reaching a prompt.
#[test]
fn review_nothing_to_review() {
    let f = Fixture::new();
    f.init();
    f.seed();
    snap_all_formats!(f, "review_nothing", &["review"]);
}

#[test]
fn review_challenged_only_when_none() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!(
        "review_challenged_only_none",
        f.run(&["review", "--challenged-only"])
    );
}

#[test]
fn review_stale_only_with_window() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!(
        "review_stale_only",
        f.run(&["review", "--stale-only", "--stale-after-days", "3650"])
    );
}
