//! Command-line interface for EngramDB.
//!
//! This module provides the CLI application for interacting with EngramDB stores.
//! It handles argument parsing, command dispatch, and output formatting.
//!
//! # Key Components
//!
//! - [`app`]: Clap-based CLI definitions and command structures.
//! - [`commands`]: Individual command handler implementations.
//! - [`output`]: Output formatting for different display modes (JSON, pretty, plain).
//!
//! # Architecture
//!
//! The CLI follows a standard dispatch pattern:
//! 1. Parse command-line arguments using Clap (in `app`).
//! 2. Create output formatter based on user preferences.
//! 3. Dispatch to appropriate command handler (in `commands`).
//! 4. Format and display results using the output formatter.

pub mod app;
pub mod commands;
pub mod output;
pub mod prompter;
pub mod validation;

/// Determine the `DaemonPolicy` for a CLI invocation based on the flag ladder.
///
/// Precedence (highest first):
/// 1. `daemon.enabled == false` → `InProcess` (master switch).
/// 2. `in_process` flag (or `ENGRAMDB_IN_PROCESS` env var, checked before this
///    call) → `InProcess`.
/// 3. `daemon.use_for_cli == false` **and** `spawn` not set → `InProcess`.
/// 4. `spawn` flag → `ConnectOrSpawn`.
/// 5. Otherwise → `ConnectOnly` (use a live daemon, else in-process fallback).
pub fn cli_daemon_policy(
    in_process: bool,
    spawn: bool,
    config: &engramdb::types::EngramConfig,
) -> engramdb::ops::DaemonPolicy {
    use engramdb::ops::DaemonPolicy;

    // Master switch: daemon globally disabled → always in-process.
    if !config.daemon.enabled {
        return DaemonPolicy::InProcess;
    }
    // --in-process flag (or ENGRAMDB_IN_PROCESS env) → in-process.
    if in_process {
        return DaemonPolicy::InProcess;
    }
    // use_for_cli=false and no explicit --spawn-daemon → in-process.
    if !config.daemon.use_for_cli && !spawn {
        return DaemonPolicy::InProcess;
    }
    // --spawn-daemon → promote to ConnectOrSpawn.
    if spawn {
        return DaemonPolicy::ConnectOrSpawn;
    }
    // Default CLI policy: use a live daemon if one exists, else in-process.
    DaemonPolicy::ConnectOnly
}

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects
// `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` to per-process temp dirs before
// any test runs. The in-crate command unit tests (e.g. `commands::get` /
// `commands::doctor` global-store cases) build real `MemoryStore`s; without
// this they would touch the *real* global data dir under nextest. The explicit
// `arm()` reference keeps the linker from dead-stripping the constructor.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn arm_test_isolation() {
    engram_test_support::arm();
}

use anyhow::Result;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use app::{Cli, Command, HookCommand};
use commands::{AddParams, ChallengeParams, QueryParams, UpdateParams};
use output::OutputFormatter;

use engramdb::ops::DaemonCell;
use engramdb::storage::FileRegistry;
use prompter::InquirePrompter;

/// Run the CLI application with parsed arguments.
///
/// This is the main entry point for the CLI. It determines the working directory,
/// creates an output formatter, and dispatches to the appropriate command handler.
///
/// # Arguments
/// * `cli` - Parsed command-line arguments
///
/// # Returns
/// Ok(()) on success, or an error if the command fails
pub async fn run(cli: Cli) -> Result<()> {
    // Initialize tracing so warnings (e.g. from reindex) are visible.
    // Defaults to WARN level; override with RUST_LOG env var.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .init();

    // Determine working directory
    let dir = cli
        .dir
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Capture embedding backend override before moving cli fields
    let backend = cli.embedding_backend;

    // Resolve the in-process flag: --in-process CLI flag OR ENGRAMDB_IN_PROCESS
    // env var (any truthy string: "1", "true", "yes", "on").
    let in_process_flag = cli.in_process || engramdb::types::in_process_override();
    let spawn_daemon_flag = cli.spawn_daemon;

    // One process-wide DaemonCell — model-needing ops share it so the daemon
    // handle is cached and health-checked once per process, not per command.
    let daemon_cell = Arc::new(DaemonCell::new());

    // Create output formatter
    let formatter = OutputFormatter::new(cli.format, cli.json, cli.no_color);

    // Create global file-backed registry for all commands
    let registry = FileRegistry::global()?;

    // If we're inside a linked git worktree, transparently route memory
    // operations to the main worktree's project: ensure it is initialized,
    // consolidate any memories that were written under this worktree's own
    // stray store, and register the worktree as a sub-project. `init` and
    // `serve` perform their own worktree handling (they own user-facing
    // messaging / run the MCP server); `completions` and `setup` don't touch
    // a memory store; `daemon` is a process-wide model host that only reads
    // `dir` for its `[daemon]` config section (each request carries its own
    // resolved store dir).
    let is_exempt = matches!(
        cli.command,
        Command::Init { .. }
            | Command::Serve { .. }
            | Command::Completions { .. }
            | Command::Setup { .. }
            | Command::Daemon { .. }
    );
    // Whether this invocation is on the main worktree (or a plain, non-worktree
    // project) rather than inside a linked git worktree — decided from the
    // *original* cwd before resolution rewrites it to the main root.
    let on_main_worktree = engramdb::storage::detect_worktree_main(&dir).is_none();
    let dir = if is_exempt {
        dir
    } else {
        engramdb::storage::worktree::resolve_project_root(&dir, &registry).await?
    };

    // Load the project config once (best-effort: defaults if absent/unreadable)
    // — used for both the maintenance policy and the daemon policy below.
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = engramdb::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();

    // When operating directly on the main worktree, run best-effort, throttled
    // housekeeping: clean up orphan/stale projects and quick-check the store's
    // health. Linked worktrees only link/consolidate (done by the resolution
    // above); the cleanup is concentrated on the main checkout. Honors the
    // `[maintenance]` config and the `--no-maintenance` flag. Failures are
    // logged and swallowed so they never block the actual command.
    if !is_exempt && on_main_worktree {
        engramdb::ops::auto_maintain(&dir, &registry, &config.maintenance, cli.no_maintenance)
            .await;
    }

    // Compute the daemon policy once per process using the project config.
    // Defaults (daemon.enabled=true, use_for_cli=true → ConnectOnly by default)
    // apply when the config file is absent/unreadable.
    let daemon_policy = cli_daemon_policy(in_process_flag, spawn_daemon_flag, &config);

    // Create production prompter for interactive commands
    let prompter = InquirePrompter;

    // Dispatch to command handlers
    match cli.command {
        Command::Init {
            no_embeddings,
            template,
        } => {
            commands::run_init(
                &dir,
                &registry,
                no_embeddings,
                template,
                backend,
                &formatter,
            )
            .await
        }
        Command::Add {
            type_,
            content,
            summary,
            title,
            physical,
            logical,
            tags,
            criticality,
            confidence,
            details,
            visibility,
            supersedes,
            decay_strategy,
            decay_half_life,
            decay_ttl,
            decay_floor,
            interactive,
            editor,
            details_file,
            global,
        } => {
            commands::add::run_add_with_daemon(
                &dir,
                global,
                &registry,
                AddParams {
                    type_str: type_,
                    content,
                    summary,
                    title,
                    physical,
                    logical,
                    tags,
                    criticality,
                    confidence,
                    details,
                    visibility_str: visibility,
                    supersedes,
                    decay_strategy,
                    decay_half_life,
                    decay_ttl,
                    decay_floor,
                    interactive,
                    editor,
                    details_file,
                },
                backend,
                &formatter,
                &prompter,
                Some(&daemon_cell),
                daemon_policy,
            )
            .await
        }
        Command::Get {
            id,
            full,
            raw,
            path,
            global,
        } => commands::run_get(&dir, global, &id, full, raw, path, &formatter).await,
        Command::Query {
            mode,
            query_pos,
            query,
            path,
            logical,
            type_,
            tags,
            min_criticality,
            max_results,
            detail_level,
            include_expired,
            show_scores,
            include_global,
            global,
        } => {
            let retrieval_mode = match mode.as_str() {
                "rank" => engramdb::retrieval::engine::RetrievalMode::Rank,
                "filter" => engramdb::retrieval::engine::RetrievalMode::Filter,
                other => {
                    return Err(anyhow::anyhow!(
                        "Invalid --mode value {:?}; expected \"rank\" or \"filter\"",
                        other
                    ));
                }
            };

            // Explicit --query wins over positional.
            let query_text = query.or(query_pos);

            commands::query::run_query_with_daemon(
                &dir,
                global,
                QueryParams {
                    mode: retrieval_mode,
                    query: query_text,
                    path,
                    logical,
                    type_filter: type_,
                    tags,
                    min_criticality,
                    max_results,
                    detail_level,
                    include_expired,
                    show_scores,
                    include_global,
                },
                backend,
                &formatter,
                &daemon_cell,
                daemon_policy,
            )
            .await
        }
        Command::List {
            type_,
            tags,
            status,
            scope,
            sort,
            reverse,
            limit,
            global,
        } => {
            commands::run_list(
                &dir,
                global,
                type_,
                tags,
                status,
                scope,
                &sort,
                reverse,
                limit,
                cli.verbose,
                &formatter,
            )
            .await
        }
        Command::Update {
            id,
            type_,
            content,
            summary,
            title,
            physical,
            logical,
            tags,
            tags_add,
            tags_remove,
            criticality,
            confidence,
            details,
            details_file,
            visibility,
            status,
            supersedes,
            decay_strategy,
            decay_half_life,
            decay_ttl,
            decay_floor,
            editor,
            global,
        } => {
            commands::update::run_update_with_daemon(
                &dir,
                global,
                UpdateParams {
                    id,
                    type_,
                    content,
                    summary,
                    title,
                    physical,
                    logical,
                    tags,
                    tags_add,
                    tags_remove,
                    criticality,
                    confidence,
                    details,
                    details_file,
                    visibility,
                    status,
                    supersedes,
                    decay_strategy,
                    decay_half_life,
                    decay_ttl,
                    decay_floor,
                    editor,
                },
                backend,
                &formatter,
                Some(&daemon_cell),
                daemon_policy,
            )
            .await
        }
        Command::Delete { id, force, global } => {
            commands::run_delete(&dir, global, &id, force, &formatter).await
        }
        Command::Stats {
            all_projects,
            global,
            daemon,
        } => commands::run_stats(&dir, global, daemon, backend, all_projects, &formatter).await,
        Command::Doctor {
            command,
            global,
            fix,
            yes,
        } => commands::run_doctor(&dir, global, command, fix, yes, &prompter, &formatter).await,
        Command::Challenge {
            id,
            evidence,
            source_file,
            global,
        } => {
            commands::run_challenge(
                &dir,
                global,
                ChallengeParams {
                    id,
                    evidence,
                    source_file,
                },
                &formatter,
            )
            .await
        }
        Command::Gc {
            confirm,
            threshold,
            global,
        } => commands::run_gc(&dir, global, confirm, threshold, &formatter).await,
        Command::Compress {
            scope,
            threshold,
            global,
        } => commands::run_compress(&dir, global, scope, threshold, &formatter).await,
        Command::Serve { transport, port } => {
            commands::run_serve(&dir, &transport, port, backend, &formatter).await
        }
        Command::Daemon { command } => commands::run_daemon_cmd(&dir, command, &formatter).await,
        Command::Completions { shell } => {
            commands::run_completions(shell);
            Ok(())
        }
        Command::Migrate { dry_run, global } => {
            let target_dir = if global {
                engramdb::storage::paths::global_store_dir()?
            } else {
                dir.clone()
            };
            commands::run_migrate(&target_dir, global, dry_run, &formatter).await
        }
        Command::Rollback {
            target_version,
            dry_run,
            global,
        } => {
            // Version 1 is represented as None internally (legacy format without
            // version field); unsupported versions are rejected (finding #22).
            let target = commands::rollback::resolve_rollback_target(target_version)?;
            let target_dir = if global {
                engramdb::storage::paths::global_store_dir()?
            } else {
                dir.clone()
            };
            commands::run_rollback(&target_dir, global, target, dry_run, &formatter).await
        }
        Command::Reindex {
            embeddings_only,
            index_only,
            global,
        } => {
            commands::reindex::run_reindex_with_daemon(
                &dir,
                global,
                embeddings_only,
                index_only,
                backend,
                &formatter,
                Some(&daemon_cell),
                daemon_policy,
            )
            .await
        }
        Command::Review {
            scope,
            type_,
            challenged_only,
            stale_only,
            global,
        } => {
            commands::run_review(
                &dir,
                global,
                scope,
                type_,
                challenged_only,
                stale_only,
                &formatter,
                &prompter,
            )
            .await
        }
        Command::Setup {
            no_plugin,
            global,
            dry_run,
            claude_dir,
        } => {
            commands::run_setup(
                &dir,
                no_plugin,
                global,
                dry_run,
                claude_dir.as_deref(),
                &formatter,
            )
            .await
        }
        Command::Hook { command } => match command {
            // Hooks fire on every Read/Write/Edit, never embed (query: None)
            // and never create memories — they deliberately skip provider
            // resolution (no `backend`), see `build_engine_without_providers`.
            HookCommand::PreToolUse => commands::run_hook_pre_tool_use(&dir).await,
            HookCommand::SessionStart { min_criticality } => {
                commands::run_hook_session_start(&dir, min_criticality).await
            }
        },
        Command::Projects { command } => {
            commands::run_projects(&dir, &registry, command, &formatter, &prompter).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::ops::DaemonPolicy;
    use engramdb::types::EngramConfig;

    #[test]
    fn cli_policy_precedence() {
        let mut c = EngramConfig::default();
        // Default: use_for_cli=true, enabled=true → ConnectOnly.
        assert_eq!(
            cli_daemon_policy(false, false, &c),
            DaemonPolicy::ConnectOnly
        );
        // --in-process → InProcess regardless of use_for_cli.
        assert_eq!(cli_daemon_policy(true, false, &c), DaemonPolicy::InProcess);
        // --spawn-daemon → ConnectOrSpawn.
        assert_eq!(
            cli_daemon_policy(false, true, &c),
            DaemonPolicy::ConnectOrSpawn
        );
        // use_for_cli=false + no --spawn-daemon → InProcess.
        c.daemon.use_for_cli = false;
        assert_eq!(cli_daemon_policy(false, false, &c), DaemonPolicy::InProcess);
        // daemon.enabled=false is the master switch — wins over --spawn-daemon.
        c.daemon.enabled = false;
        assert_eq!(cli_daemon_policy(false, true, &c), DaemonPolicy::InProcess);
    }
}
