//! `init`, `add`, `get`, `list`, `update`, `delete`, `verify`, `task`.
//!
//! Default format only (a pipe resolves to JSON — see `OutputFormatter::new`).
//! These commands render through `OutputFormatter`, so their pretty/plain
//! layouts are tier 1's job; what is asserted here is the wiring: that a flag
//! reaches the renderer at all, that the exit code matches the outcome, and
//! that human chatter stays off stdout when the format is JSON.

use super::Fixture;

// =====================================================================
// init
// =====================================================================

#[test]
fn init_fresh_store() {
    let f = Fixture::new();
    let template = f.path().join("cfg.toml");
    std::fs::write(&template, "[title]\nstrategy = \"keyword\"\n").unwrap();
    insta::assert_snapshot!(
        "init_fresh_store",
        f.run(&[
            "init",
            "--no-embeddings",
            "--template",
            template.to_str().unwrap()
        ])
    );
}

#[test]
fn init_already_initialized() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "init_already_initialized",
        f.run(&["init", "--no-embeddings"])
    );
}

#[test]
fn init_missing_template_fails() {
    let f = Fixture::new();
    insta::assert_snapshot!(
        "init_missing_template_fails",
        f.run(&[
            "init",
            "--no-embeddings",
            "--template",
            "/no/such/template.toml"
        ])
    );
}

// =====================================================================
// add
// =====================================================================

#[test]
fn add_minimal() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_minimal",
        f.run(&[
            "add",
            "-t",
            "decision",
            "-s",
            "A decision",
            "-c",
            "Because."
        ])
    );
}

#[test]
fn add_with_all_scoping_flags() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_with_all_scoping_flags",
        f.run(&[
            "add",
            "-t",
            "hazard",
            "-s",
            "Do not block the async runtime",
            "-c",
            "Blocking calls in an async context deadlock the daemon.",
            "-p",
            "src/daemon/server.rs",
            "-l",
            "daemon.runtime",
            "--tags",
            "async,deadlock",
            "--criticality",
            "0.95",
            "--confidence",
            "0.7",
            "--details",
            "Seen once in production.",
        ])
    );
}

#[test]
fn add_content_as_positional() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_content_as_positional",
        f.run(&[
            "add",
            "-t",
            "convention",
            "-s",
            "Quick start form",
            "Positional content."
        ])
    );
}

#[test]
fn add_missing_args_non_tty_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_missing_args_non_tty_fails",
        f.run(&["add", "-t", "decision"])
    );
}

#[test]
fn add_invalid_type_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_invalid_type_fails",
        f.run(&["add", "-t", "not-a-real-type", "-s", "S", "-c", "C"])
    );
}

#[test]
fn add_criticality_above_range_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_criticality_above_range_fails",
        f.run(&[
            "add",
            "-t",
            "decision",
            "-s",
            "S",
            "-c",
            "C",
            "--criticality",
            "2.0"
        ])
    );
}

#[test]
fn add_summary_too_long_fails() {
    let f = Fixture::new();
    f.init();
    let long = "A".repeat(500);
    insta::assert_snapshot!(
        "add_summary_too_long_fails",
        f.run(&["add", "-t", "decision", "-s", &long, "-c", "C"])
    );
}

#[test]
fn add_missing_details_file_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "add_missing_details_file_fails",
        f.run(&[
            "add",
            "-t",
            "decision",
            "-s",
            "S",
            "-c",
            "C",
            "--details-file",
            "/no/such/details.txt"
        ])
    );
}

/// `add` auto-creates the store rather than failing — unlike `list`, which
/// errors with "Project not initialized" (see below). The asymmetry is
/// deliberate on the write path, and worth pinning so it cannot drift
/// unnoticed in either direction.
#[test]
fn add_auto_initializes_the_store() {
    let f = Fixture::new();
    f.write_config_only();
    insta::assert_snapshot!(
        "add_auto_initializes_the_store",
        f.run(&["add", "-t", "decision", "-s", "S", "-c", "C"])
    );
}

// =====================================================================
// get
// =====================================================================

/// Add one memory and hand back its id.
fn seed_one(f: &Fixture) -> String {
    let out = f.run(&[
        "add",
        "-t",
        "decision",
        "-s",
        "The one memory",
        "-c",
        "Its content.",
        "--tags",
        "alpha,beta",
    ]);
    // The transcript is normalized, so recover the id from the store instead.
    let _ = out;
    let dir = f.path().join(".engramdb/memories");
    let entry = std::fs::read_dir(&dir)
        .expect("memories dir")
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|x| x == "md"))
        .expect("one memory file");
    let stem = entry
        .path()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    stem.rsplit('_').next().unwrap().to_string()
}

#[test]
fn get_existing() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!("get_existing", f.run(&["get", &id]));
}

#[test]
fn get_full() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!("get_full", f.run(&["get", &id, "--full"]));
}

#[test]
fn get_raw() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!("get_raw", f.run(&["get", &id, "--raw"]));
}

#[test]
fn get_path_only() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!("get_path_only", f.run(&["get", &id, "--path"]));
}

#[test]
fn get_unknown_id_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("get_unknown_id_fails", f.run(&["get", "no-such-memory"]));
}

// =====================================================================
// list
// =====================================================================

#[test]
fn list_empty_store() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("list_empty_store", f.run(&["list"]));
}

#[test]
fn list_seeded() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!("list_seeded", f.run(&["list"]));
}

#[test]
fn list_filtered_by_type() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!("list_filtered_by_type", f.run(&["list", "-t", "hazard"]));
}

#[test]
fn list_sorted_reverse() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!(
        "list_sorted_reverse",
        f.run(&["list", "--sort", "criticality", "--reverse"])
    );
}

#[test]
fn list_limited() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!("list_limited", f.run(&["list", "-n", "2"]));
}

#[test]
fn list_verbose() {
    let f = Fixture::new();
    f.init();
    f.seed();
    insta::assert_snapshot!("list_verbose", f.run(&["--verbose", "list"]));
}

/// The read-path counterpart to `add_auto_initializes_the_store`: `list`
/// refuses rather than creating anything.
#[test]
fn list_on_uninitialized_store_fails() {
    let f = Fixture::new();
    f.write_config_only();
    insta::assert_snapshot!("list_on_uninitialized_store_fails", f.run(&["list"]));
}

// =====================================================================
// update / delete / verify
// =====================================================================

#[test]
fn update_summary() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!(
        "update_summary",
        f.run(&["update", &id, "-s", "A new summary"])
    );
}

#[test]
fn update_tags_add_and_remove() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!(
        "update_tags_add_and_remove",
        f.run(&[
            "update",
            &id,
            "--tags-add",
            "gamma",
            "--tags-remove",
            "alpha"
        ])
    );
}

#[test]
fn update_unknown_id_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "update_unknown_id_fails",
        f.run(&["update", "no-such-memory", "-s", "X"])
    );
}

#[test]
fn delete_existing() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!("delete_existing", f.run(&["delete", &id, "--force"]));
}

#[test]
fn delete_unknown_id_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "delete_unknown_id_fails",
        f.run(&["delete", "no-such-memory", "--force"])
    );
}

#[test]
fn verify_existing() {
    let f = Fixture::new();
    f.init();
    let id = seed_one(&f);
    insta::assert_snapshot!("verify_existing", f.run(&["verify", &id]));
}

#[test]
fn verify_unknown_id_fails() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "verify_unknown_id_fails",
        f.run(&["verify", "no-such-memory"])
    );
}

// =====================================================================
// task
// =====================================================================

#[test]
fn task_current_when_unset() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!("task_current_when_unset", f.run(&["task", "current"]));
}

#[test]
fn task_current_set() {
    let f = Fixture::new();
    f.init();
    insta::assert_snapshot!(
        "task_current_set",
        f.run(&["task", "current", "refactor-output"])
    );
}

#[test]
fn task_complete() {
    let f = Fixture::new();
    f.init();
    f.run(&["task", "current", "refactor-output"]);
    insta::assert_snapshot!(
        "task_complete",
        f.run(&["task", "complete", "refactor-output"])
    );
}
