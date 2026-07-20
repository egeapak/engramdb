//! Handler for the `engramdb groups` subcommand.
//!
//! Groups are the multi-project memory tier: a named, machine-local store that
//! a set of projects subscribe to (between one project and the machine-wide
//! global store). Membership is recorded in `registry.json` as each project's
//! `subscriptions` list; the group store itself lives under the global data
//! dir (see `storage::paths::group_store_dir`). Subscribed groups fan into a
//! project's queries by default, and a member project may write to the group
//! without tripping the MCP cross-project write gate.

use crate::app::GroupsCommand;
use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::storage::paths::compute_group_id;
use engramdb::storage::project_id::compute_project_id;
use engramdb::storage::{registry::subscriptions_of, RegistryBackend};
use std::path::Path;

/// Run the `groups` subcommand.
///
/// `dir` is the already-resolved project root (worktree routing is applied
/// upstream in `cli::run`, so membership always attaches to the main project's
/// id — never a linked worktree's stray id). `registry` is the shared
/// file-backed registry all commands receive.
pub async fn run_groups(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: GroupsCommand,
    formatter: &OutputFormatter,
) -> Result<()> {
    match command {
        GroupsCommand::Create { name } => {
            // Idempotent: create_group returns the stable id whether or not the
            // group already existed.
            let gid = registry.create_group(&name).await?;
            formatter.print_success(&format!("Group '{name}' ready (id: {gid})."));
        }
        GroupsCommand::Subscribe { name } => {
            let gid = compute_group_id(&name);
            // Ensure the group exists before subscribing (subscribing to a
            // never-created group would otherwise leave a dangling id).
            registry.create_group(&name).await?;
            let project_id = compute_project_id(dir);
            registry.subscribe(&project_id, &gid).await?;
            formatter.print_success(&format!(
                "Subscribed project '{project_id}' to group '{name}' (id: {gid})."
            ));
        }
        GroupsCommand::Unsubscribe { name } => {
            let gid = compute_group_id(&name);
            let project_id = compute_project_id(dir);
            // Forgiving: unsubscribe is a no-op if the project was not subscribed.
            registry.unsubscribe(&project_id, &gid).await?;
            formatter.print_success(&format!(
                "Unsubscribed project '{project_id}' from group '{name}' (id: {gid})."
            ));
        }
        GroupsCommand::List => {
            let reg = registry.load().await?;
            let project_id = compute_project_id(dir);
            let subscribed = subscriptions_of(&reg, &project_id);

            if reg.groups.is_empty() {
                formatter.print_message(
                    "No groups defined. Create one with `engramdb groups create <name>`.",
                );
                return Ok(());
            }

            formatter.print_message("Groups:");
            for group in &reg.groups {
                let mark = if subscribed.iter().any(|g| g == &group.group_id) {
                    " (subscribed)"
                } else {
                    ""
                };
                formatter.print_message(&format!("  {} — {}{mark}", group.name, group.group_id));
            }
            if subscribed.is_empty() {
                formatter.print_hint(&format!(
                    "Project '{project_id}' is not subscribed to any group."
                ));
            }
        }
    }
    Ok(())
}
