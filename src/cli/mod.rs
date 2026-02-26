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

use anyhow::Result;
use std::env;
use std::path::PathBuf;

use app::{Cli, Command, HookCommand};
use commands::{AddParams, ChallengeParams, RetrieveParams, SearchParams, UpdateParams};
use output::OutputFormatter;

use crate::storage::FileRegistry;
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

    // Create output formatter
    let formatter = OutputFormatter::new(cli.format, cli.json, cli.no_color);

    // Create global file-backed registry for all commands
    let registry = FileRegistry::global()?;

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
        } => {
            commands::run_add(
                &dir,
                &registry,
                AddParams {
                    type_str: type_,
                    content,
                    summary,
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
            )
            .await
        }
        Command::Get {
            id,
            full,
            raw,
            path,
        } => commands::run_get(&dir, &id, full, raw, path, &formatter).await,
        Command::Retrieve {
            path,
            logical,
            query,
            type_,
            tags,
            min_criticality,
            max_results,
            detail_level,
            include_expired,
            show_scores,
        } => {
            commands::run_retrieve(
                &dir,
                RetrieveParams {
                    path,
                    logical,
                    query,
                    type_filter: type_,
                    tags,
                    min_criticality,
                    max_results,
                    detail_level,
                    include_expired,
                    show_scores,
                },
                backend,
                &formatter,
            )
            .await
        }
        Command::Search {
            query,
            type_,
            tags,
            physical,
            logical,
            min_criticality,
            max_results,
        } => {
            commands::run_search(
                &dir,
                SearchParams {
                    query,
                    type_filter: type_,
                    tags,
                    physical,
                    logical,
                    min_criticality,
                    max_results,
                },
                backend,
                &formatter,
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
        } => {
            commands::run_list(
                &dir,
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
        } => {
            commands::run_update(
                &dir,
                UpdateParams {
                    id,
                    type_,
                    content,
                    summary,
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
            )
            .await
        }
        Command::Delete { id, force } => commands::run_delete(&dir, &id, force, &formatter).await,
        Command::Stats => commands::run_stats(&dir, backend, &formatter).await,
        Command::Doctor { command } => commands::run_doctor(&dir, command, &formatter).await,
        Command::Challenge {
            id,
            evidence,
            source_file,
        } => {
            commands::run_challenge(
                &dir,
                ChallengeParams {
                    id,
                    evidence,
                    source_file,
                },
                &formatter,
            )
            .await
        }
        Command::Gc { confirm, threshold } => {
            commands::run_gc(&dir, confirm, threshold, &formatter).await
        }
        Command::Compress { scope, threshold } => {
            commands::run_compress(&dir, scope, threshold, &formatter).await
        }
        Command::Serve { transport, port } => {
            commands::run_serve(&dir, &transport, port, backend, &formatter).await
        }
        Command::Completions { shell } => {
            commands::run_completions(shell);
            Ok(())
        }
        Command::Reindex {
            embeddings_only,
            index_only,
        } => commands::run_reindex(&dir, embeddings_only, index_only, backend, &formatter).await,
        Command::Review {
            scope,
            type_,
            challenged_only,
            stale_only,
        } => {
            commands::run_review(
                &dir,
                scope,
                type_,
                challenged_only,
                stale_only,
                &formatter,
                &prompter,
            )
            .await
        }
        Command::Hook { command } => match command {
            HookCommand::PreToolUse => commands::run_hook_pre_tool_use(&dir, backend).await,
        },
        Command::Projects { command } => {
            commands::run_projects(&dir, &registry, command, &formatter, &prompter).await
        }
    }
}
