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

use anyhow::Result;
use std::env;
use std::path::PathBuf;

use app::{Cli, Command};
use commands::{AddParams, ChallengeParams, RetrieveParams, SearchParams, UpdateParams};
use output::OutputFormatter;

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
    // Determine working directory
    let dir = cli
        .dir
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Create output formatter
    let formatter = OutputFormatter::new(cli.format, cli.json, cli.no_color);

    // Dispatch to command handlers
    match cli.command {
        Command::Init {
            no_embeddings,
            template,
        } => commands::run_init(&dir, no_embeddings, template, &formatter).await,
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
            interactive,
            editor,
            details_file,
        } => {
            commands::run_add(
                &dir,
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
                    interactive,
                    editor,
                    details_file,
                },
                &formatter,
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
                &dir, type_, tags, status, scope, &sort, reverse, limit, &formatter,
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
                    editor,
                },
                &formatter,
            )
            .await
        }
        Command::Delete { id, force } => commands::run_delete(&dir, &id, force, &formatter).await,
        Command::Stats => commands::run_stats(&dir, &formatter).await,
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
            commands::run_serve(&dir, &transport, port, &formatter).await
        }
        Command::Completions { shell } => {
            commands::run_completions(shell);
            Ok(())
        }
        Command::Reindex {
            embeddings_only,
            index_only,
        } => commands::run_reindex(&dir, embeddings_only, index_only, &formatter).await,
        Command::Review {
            scope,
            type_,
            challenged_only,
            stale_only,
        } => {
            commands::run_review(&dir, scope, type_, challenged_only, stale_only, &formatter).await
        }
    }
}
