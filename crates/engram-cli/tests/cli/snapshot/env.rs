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
