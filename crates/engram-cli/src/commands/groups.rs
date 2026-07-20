//! Handler for the `engramdb groups` subcommand.
//!
//! Groups are the multi-project memory tier: a named, machine-local store that
//! a set of projects subscribe to (between one project and the machine-wide
//! global store). Membership is recorded in `registry.json` as each project's
//! `subscriptions` list; the group store itself lives under the global data
//! dir (see `storage::paths::group_store_dir`). Subscribed groups fan into a
//! project's queries by default, and a member project may write to the group
//! without tripping the MCP cross-project write gate.
//!
//! Subscribe/unsubscribe print the **blast radius** (how many memories fold in
//! or drop out, and how many projects share the group) before mutating, so a
//! membership change is never silent. `--yes` skips the prompt for scripting;
//! JSON mode refuses to prompt and requires `--yes`.

use crate::app::GroupsCommand;
use crate::output::OutputFormatter;
use crate::prompter::Prompter;
use anyhow::Result;
use engramdb::storage::paths::compute_group_id;
use engramdb::storage::project_id::compute_project_id;
use engramdb::storage::{
    registry::{subscribers_of, subscriptions_of},
    MemoryStore, RegistryBackend,
};
use std::path::Path;

/// Count the memories a group store holds without creating it.
///
/// Returns `Ok(None)` when the group store has never been written to (no store
/// dir yet — blast radius is zero), `Ok(Some(n))` for `n` memories, and `Err`
/// only when the store exists but is unreadable/corrupt. We check for the store
/// dir first rather than calling `open_group` unconditionally, because
/// `open_group` *creates* an empty store as a side effect — undesirable from
/// the read-only `members`/blast-radius paths.
async fn group_memory_count(group_id: &str) -> Result<Option<usize>> {
    let store_dir = engramdb::storage::paths::group_store_dir(group_id)?;
    let engramdb_dir = engramdb::storage::paths::project_dir(&store_dir);
    if !engramdb_dir.exists() {
        return Ok(None);
    }
    let store = MemoryStore::open_group(group_id).await?;
    Ok(Some(store.count().await?))
}

/// Render a group's memory count for a human message: an explicit "0" when the
/// store exists and is empty, "0 (not yet created)" when it has never been
/// written, or "unreadable" when the store is corrupt (best-effort — the
/// confirmation still proceeds).
async fn describe_memory_count(group_id: &str) -> String {
    match group_memory_count(group_id).await {
        Ok(Some(n)) => format!("{n} {}", if n == 1 { "memory" } else { "memories" }),
        Ok(None) => "0 memories (store not yet created)".to_string(),
        Err(_) => "an unreadable number of memories (store may be corrupt)".to_string(),
    }
}

/// Run the `groups` subcommand.
///
/// `dir` is the already-resolved project root (worktree routing is applied
/// upstream in `cli::run`, so membership always attaches to the main project's
/// id — never a linked worktree's stray id). `registry` is the shared
/// file-backed registry all commands receive. `prompter` drives the
/// blast-radius confirmations.
pub async fn run_groups(
    dir: &Path,
    registry: &dyn RegistryBackend,
    command: GroupsCommand,
    prompter: &dyn Prompter,
    formatter: &OutputFormatter,
) -> Result<()> {
    match command {
        GroupsCommand::Create { name } => {
            // Idempotent: create_group returns the stable id whether or not the
            // group already existed.
            let gid = registry.create_group(&name).await?;
            formatter.print_success(&format!("Group '{name}' ready (id: {gid})."));
        }
        GroupsCommand::Subscribe { name, yes } => {
            let gid = compute_group_id(&name);
            let project_id = compute_project_id(dir);

            // Already subscribed? Nothing changes — report and return without a
            // pointless confirmation.
            let reg = registry.load().await?;
            if subscriptions_of(&reg, &project_id)
                .iter()
                .any(|g| g == &gid)
            {
                drop(reg);
                formatter.print_message(&format!(
                    "Project '{project_id}' is already subscribed to '{name}'."
                ));
                return Ok(());
            }
            let subscriber_count = subscribers_of(&reg, &gid).len();
            drop(reg);

            // Blast radius: how many memories will start folding into this
            // project's queries, and how many projects already share the group.
            if !yes {
                if formatter.is_json() {
                    anyhow::bail!(
                        "groups subscribe requires confirmation; re-run with --yes in JSON mode"
                    );
                }
                let count_desc = describe_memory_count(&gid).await;
                formatter.print_warning(&format!(
                    "Subscribing project '{project_id}' to group '{name}' will fold {count_desc} \
                     into this project's queries, and let this project write to the group \
                     (visible to all {} current subscriber(s)).",
                    subscriber_count
                ));
                if !prompter.confirm("Continue?", true).unwrap_or(false) {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

            // Ensure the group exists before subscribing (subscribing to a
            // never-created group would otherwise leave a dangling id).
            registry.create_group(&name).await?;
            registry.subscribe(&project_id, &gid).await?;
            formatter.print_success(&format!(
                "Subscribed project '{project_id}' to group '{name}' (id: {gid})."
            ));
        }
        GroupsCommand::Unsubscribe { name, yes } => {
            let gid = compute_group_id(&name);
            let project_id = compute_project_id(dir);

            // Not subscribed? unsubscribe is a forgiving no-op; say so and skip
            // the prompt entirely (the desired end state already holds).
            let reg = registry.load().await?;
            if !subscriptions_of(&reg, &project_id)
                .iter()
                .any(|g| g == &gid)
            {
                drop(reg);
                formatter.print_message(&format!(
                    "Project '{project_id}' is not subscribed to '{name}'; nothing to do."
                ));
                return Ok(());
            }
            drop(reg);

            if !yes {
                if formatter.is_json() {
                    anyhow::bail!(
                        "groups unsubscribe requires confirmation; re-run with --yes in JSON mode"
                    );
                }
                let count_desc = describe_memory_count(&gid).await;
                formatter.print_warning(&format!(
                    "Unsubscribing will stop project '{project_id}' from seeing group '{name}'s \
                     {count_desc} in its queries. The group and its memories are not deleted."
                ));
                if !prompter.confirm("Continue?", true).unwrap_or(false) {
                    formatter.print_message("Aborted.");
                    return Ok(());
                }
            }

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
        GroupsCommand::Members { name } => {
            let gid = compute_group_id(&name);
            let reg = registry.load().await?;
            let members: Vec<String> = subscribers_of(&reg, &gid)
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            let known = reg.groups.iter().any(|g| g.group_id == gid);
            drop(reg);

            let count_desc = describe_memory_count(&gid).await;
            if !known {
                formatter.print_message(&format!(
                    "Group '{name}' (id: {gid}) is not in the registry. Create it with \
                     `engramdb groups create {name}`."
                ));
                return Ok(());
            }

            formatter.print_message(&format!("Group '{name}' (id: {gid}) holds {count_desc}."));
            if members.is_empty() {
                formatter.print_hint("No projects are subscribed to this group yet.");
            } else {
                formatter.print_message(&format!("Subscribed projects ({}):", members.len()));
                for pid in members {
                    formatter.print_message(&format!("  {pid}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use crate::output::OutputFormatter;
    use crate::prompter::MockPrompter;
    use engramdb::storage::InMemoryRegistry;
    use tempfile::TempDir;

    // Force Plain so `is_json()` is false regardless of TTY — nextest runs
    // without a TTY, where `new(None, false, _)` would resolve to JSON and take
    // the non-interactive bail path instead of exercising the prompt.
    fn fmt() -> OutputFormatter {
        OutputFormatter::new(Some(OutputFormat::Plain), false, true)
    }

    async fn registered_project(reg: &InMemoryRegistry, dir: &Path) -> String {
        let pid = compute_project_id(dir);
        reg.update(dir, &pid).await.unwrap();
        pid
    }

    #[tokio::test]
    async fn subscribe_declined_does_not_subscribe() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let pid = registered_project(&reg, dir.path()).await;

        let prompter = MockPrompter::new(vec!["no"]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Subscribe {
                name: "grp".into(),
                yes: false,
            },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();

        let loaded = reg.load().await.unwrap();
        assert!(
            subscriptions_of(&loaded, &pid).is_empty(),
            "declining the confirmation must not subscribe"
        );
    }

    #[tokio::test]
    async fn subscribe_confirmed_subscribes() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let pid = registered_project(&reg, dir.path()).await;

        let prompter = MockPrompter::new(vec!["yes"]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Subscribe {
                name: "grp".into(),
                yes: false,
            },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();

        let loaded = reg.load().await.unwrap();
        let gid = compute_group_id("grp");
        assert!(subscriptions_of(&loaded, &pid).iter().any(|g| g == &gid));
    }

    #[tokio::test]
    async fn subscribe_yes_flag_skips_prompt() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let pid = registered_project(&reg, dir.path()).await;

        // Empty queue: were the prompt reached, confirm() would return Err and
        // the handler would abort (not subscribe). --yes must bypass it.
        let prompter = MockPrompter::new(vec![]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Subscribe {
                name: "grp".into(),
                yes: true,
            },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();

        let loaded = reg.load().await.unwrap();
        let gid = compute_group_id("grp");
        assert!(
            subscriptions_of(&loaded, &pid).iter().any(|g| g == &gid),
            "--yes must subscribe without prompting"
        );
    }

    #[tokio::test]
    async fn subscribe_json_mode_without_yes_errors() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        registered_project(&reg, dir.path()).await;

        let json_fmt = OutputFormatter::new(None, true, true);
        let prompter = MockPrompter::new(vec![]);
        let err = run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Subscribe {
                name: "grp".into(),
                yes: false,
            },
            &prompter,
            &json_fmt,
        )
        .await
        .expect_err("JSON mode must refuse to prompt");
        assert!(format!("{err}").contains("--yes"));
    }

    #[tokio::test]
    async fn already_subscribed_is_noop_without_prompt() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let pid = registered_project(&reg, dir.path()).await;
        let gid = compute_group_id("grp");
        reg.create_group("grp").await.unwrap();
        reg.subscribe(&pid, &gid).await.unwrap();

        // Empty queue: the "already subscribed" short-circuit must return before
        // any prompt.
        let prompter = MockPrompter::new(vec![]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Subscribe {
                name: "grp".into(),
                yes: false,
            },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unsubscribe_not_subscribed_is_noop_without_prompt() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        registered_project(&reg, dir.path()).await;

        // Empty queue: not subscribed → forgiving no-op, no prompt.
        let prompter = MockPrompter::new(vec![]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Unsubscribe {
                name: "grp".into(),
                yes: false,
            },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unsubscribe_confirmed_removes_subscription() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let pid = registered_project(&reg, dir.path()).await;
        let gid = compute_group_id("grp");
        reg.create_group("grp").await.unwrap();
        reg.subscribe(&pid, &gid).await.unwrap();

        let prompter = MockPrompter::new(vec!["yes"]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Unsubscribe {
                name: "grp".into(),
                yes: false,
            },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();

        let loaded = reg.load().await.unwrap();
        assert!(subscriptions_of(&loaded, &pid).is_empty());
    }

    #[tokio::test]
    async fn members_runs_for_known_group() {
        let dir = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let pid = registered_project(&reg, dir.path()).await;
        let gid = compute_group_id("grp");
        reg.create_group("grp").await.unwrap();
        reg.subscribe(&pid, &gid).await.unwrap();

        // Smoke: the read-only path must not error and must not need a prompt.
        let prompter = MockPrompter::new(vec![]);
        run_groups(
            dir.path(),
            &reg,
            GroupsCommand::Members { name: "grp".into() },
            &prompter,
            &fmt(),
        )
        .await
        .unwrap();
    }
}
