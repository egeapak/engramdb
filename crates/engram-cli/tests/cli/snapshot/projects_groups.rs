//! `projects` (7 subcommands) and `groups` (5 subcommands).
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
