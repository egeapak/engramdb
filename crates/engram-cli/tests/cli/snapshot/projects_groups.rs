//! `projects` (9 subcommands) and `groups` (5 subcommands).
//!
//! Group ids are deliberately left unredacted by the normalizer: unlike
//! project ids, which hash the canonical path and so change with the temp
//! dir, a group id is `__g_` plus a hash of the group *name* and is identical
//! on every machine. Seeing the real value in the snapshot is the point.

use super::Fixture;

fn registered() -> Fixture {
    let f = Fixture::new();
    f.init();
    f.seed();
    f
}

// =====================================================================
// projects
// =====================================================================

#[test]
fn projects_info() {
    let f = registered();
    snap_all_formats!(f, "projects_info", &["projects", "info"]);
}

/// No subcommand defaults to `info`.
#[test]
fn projects_bare_defaults_to_info() {
    let f = registered();
    insta::assert_snapshot!("projects_bare", f.run(&["projects"]));
}

#[test]
fn projects_list() {
    let f = registered();
    snap_all_formats!(f, "projects_list", &["projects", "list"]);
}

#[test]
fn projects_list_grouping_modes() {
    let f = registered();
    for mode in ["always", "auto", "none"] {
        insta::assert_snapshot!(
            format!("projects_list_group_{mode}"),
            f.run(&["projects", "list", "--group", mode])
        );
    }
}

#[test]
fn projects_stats() {
    let f = registered();
    snap_all_formats!(f, "projects_stats", &["projects", "stats"]);
}

#[test]
fn projects_prune() {
    let f = registered();
    insta::assert_snapshot!("projects_prune", f.run(&["projects", "prune", "--force"]));
}

#[test]
fn projects_delete_unknown_fails() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_delete_unknown_fails",
        f.run(&["projects", "delete", "0000000000000000", "--force"])
    );
}

/// Deleting keeps the data directory whole unless `--purge` is asked for.
///
/// The pair is the point. A project ID derived from a git remote is shared by
/// every clone of that remote, and the registry records only one of them, so
/// an unqualified delete cannot know whether the personal memories it is about
/// to remove are the last copy. The default answers that by keeping them, and
/// says so; `--purge` is the explicit opt-out. Both messages are the contract.
#[test]
fn projects_delete_keeps_data_without_purge() {
    let f = registered();
    f.seed_personal("Personal note", "Only copy lives in the global data dir");
    let id = f.project_id();
    insta::assert_snapshot!(
        "projects_delete_no_purge",
        f.run(&["--format", "plain", "projects", "delete", &id, "--force"])
    );
}

#[test]
fn projects_delete_purge_removes_everything() {
    let f = registered();
    f.seed_personal("Personal note", "Only copy lives in the global data dir");
    let id = f.project_id();
    insta::assert_snapshot!(
        "projects_delete_purge",
        f.run(&["--format", "plain", "projects", "delete", &id, "--force", "--purge"])
    );
}

// ---- discover --------------------------------------------------------

/// Every store in the tree is registered, so there is nothing to adopt.
#[test]
fn discover_finds_nothing_when_all_registered() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_discover_nothing",
        f.run(&["--format", "plain", "projects", "discover", "--dry-run"])
    );
}

/// The state `discover` exists for: stores on disk that the registry has lost.
///
/// Clearing the registry is the documented trigger ("or losing
/// `registry.json`") and it makes *both* stores invisible, so the scan has
/// more than one thing to report.
#[test]
fn discover_dry_run_lists_unregistered_projects() {
    let f = registered();
    f.init_nested("nested");
    f.deregister();
    insta::assert_snapshot!(
        "projects_discover_dry_run",
        f.run(&["--format", "plain", "projects", "discover", "--dry-run"])
    );
}

/// `--yes --no-index` adopts them without prompting and without a model.
///
/// `--no-index` is what keeps this deterministic: the fixture has an empty
/// model cache and `ENGRAMDB_OFFLINE`, so an index rebuild would report a
/// provider failure whose wording is the machine's. Registration is the part
/// under test.
#[test]
fn discover_registers_what_it_finds() {
    let f = registered();
    f.init_nested("nested");
    f.deregister();
    insta::assert_snapshot!(
        "projects_discover_registers",
        f.run(&[
            "--format",
            "plain",
            "projects",
            "discover",
            "--yes",
            "--no-index"
        ])
    );
    // The registry is the outcome, so assert on it rather than the prose.
    insta::assert_snapshot!(
        "projects_discover_registers_after",
        f.run(&["projects", "list"])
    );
}

/// `--max-depth 0` stays at the scan root, so the nested store is out of reach.
#[test]
fn discover_respects_max_depth() {
    let f = registered();
    f.init_nested("nested");
    f.deregister();
    insta::assert_snapshot!(
        "projects_discover_max_depth",
        f.run(&[
            "--format",
            "plain",
            "projects",
            "discover",
            "--dry-run",
            "--max-depth",
            "0"
        ])
    );
}

/// JSON is machine-consumed, so it must never reach a prompt. Checked before
/// the scan — rejecting the arguments after walking a home directory would be
/// work nobody asked for.
#[test]
fn discover_in_json_mode_requires_yes_or_dry_run() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_discover_json_needs_yes",
        f.run(&["--format", "json", "projects", "discover"])
    );
}

// ---- repair ----------------------------------------------------------

/// The common case: the ID the project hashes to is the one on file.
#[test]
fn repair_reports_a_consistent_registration() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_repair_consistent",
        f.run(&["--format", "plain", "projects", "repair", "--force"])
    );
}

/// The drift `repair` exists for: a git remote added *after* `init`.
///
/// The project now hashes to an ID derived from the origin URL while the
/// registry still holds the path-derived one — so its memories drop out of
/// queries and its personal memories become unreachable. `--no-index` keeps
/// the fixture off the embedding path; the re-keying is what is under test.
///
/// The personal memory is seeded deliberately. Personal memories live under
/// `<data>/projects/<id>/`, so the ID changing is exactly what strands them,
/// and carrying them to the new ID is the part of `repair` that moves data
/// rather than registry rows — with an empty store the report would say
/// "copy 0 personal memory file(s)" and pin nothing.
#[test]
fn repair_re_keys_a_project_that_gained_a_git_remote() {
    let f = registered();
    f.seed_personal("Personal note", "Lives under the old project id");
    f.write_git_remote("https://github.com/example/engramdb.git");
    insta::assert_snapshot!(
        "projects_repair_rekeys",
        f.run(&[
            "--format",
            "plain",
            "projects",
            "repair",
            "--force",
            "--no-index"
        ])
    );
    insta::assert_snapshot!("projects_repair_rekeys_after", f.run(&["projects", "list"]));
}

/// Same JSON contract as `discover` and `delete`.
#[test]
fn repair_in_json_mode_requires_force() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_repair_json_needs_force",
        f.run(&["--format", "json", "projects", "repair"])
    );
}

#[test]
fn projects_link_unknown_fails() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_link_unknown_fails",
        f.run(&[
            "projects",
            "link",
            "0000000000000000",
            "--parent",
            "1111111111111111"
        ])
    );
}

#[test]
fn projects_unlink_unknown_fails() {
    let f = registered();
    insta::assert_snapshot!(
        "projects_unlink_unknown_fails",
        f.run(&["projects", "unlink", "0000000000000000"])
    );
}

// =====================================================================
// groups
// =====================================================================

#[test]
fn groups_list_empty() {
    let f = registered();
    insta::assert_snapshot!("groups_list_empty", f.run(&["groups", "list"]));
}

#[test]
fn groups_create() {
    let f = registered();
    insta::assert_snapshot!("groups_create", f.run(&["groups", "create", "backend"]));
}

#[test]
fn groups_create_twice() {
    let f = registered();
    f.run(&["groups", "create", "backend"]);
    insta::assert_snapshot!(
        "groups_create_twice",
        f.run(&["groups", "create", "backend"])
    );
}

#[test]
fn groups_subscribe_then_list() {
    let f = registered();
    f.run(&["groups", "create", "backend"]);
    insta::assert_snapshot!(
        "groups_subscribe",
        f.run(&["groups", "subscribe", "backend", "--yes"])
    );
    insta::assert_snapshot!("groups_list_after_subscribe", f.run(&["groups", "list"]));
}

#[test]
fn groups_members() {
    let f = registered();
    f.run(&["groups", "create", "backend"]);
    f.run(&["groups", "subscribe", "backend", "--yes"]);
    insta::assert_snapshot!("groups_members", f.run(&["groups", "members", "backend"]));
}

#[test]
fn groups_unsubscribe() {
    let f = registered();
    f.run(&["groups", "create", "backend"]);
    f.run(&["groups", "subscribe", "backend", "--yes"]);
    insta::assert_snapshot!(
        "groups_unsubscribe",
        f.run(&["groups", "unsubscribe", "backend", "--yes"])
    );
}

#[test]
fn groups_members_unknown_group() {
    let f = registered();
    insta::assert_snapshot!(
        "groups_members_unknown",
        f.run(&["groups", "members", "no-such-group"])
    );
}

#[test]
fn groups_subscribe_unknown_group() {
    let f = registered();
    insta::assert_snapshot!(
        "groups_subscribe_unknown",
        f.run(&["groups", "subscribe", "no-such-group", "--yes"])
    );
}
