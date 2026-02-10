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
pub fn run(cli: Cli) -> Result<()> {
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
        } => commands::run_init(&dir, no_embeddings, template, &formatter),
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
        } => commands::run_add(
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
        ),
        Command::Get {
            id,
            full,
            raw,
            path,
        } => commands::run_get(&dir, &id, full, raw, path, &formatter),
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
        } => commands::run_retrieve(
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
        ),
        Command::Search {
            query,
            type_,
            tags,
            physical,
            logical,
            min_criticality,
            max_results,
        } => commands::run_search(
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
        ),
        Command::List {
            type_,
            tags,
            status,
            scope,
            sort,
            reverse,
            limit,
        } => commands::run_list(
            &dir, type_, tags, status, scope, &sort, reverse, limit, &formatter,
        ),
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
        } => commands::run_update(
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
        ),
        Command::Delete { id, force } => commands::run_delete(&dir, &id, force, &formatter),
        Command::Stats => commands::run_stats(&dir, &formatter),
        Command::Challenge {
            id,
            evidence,
            source_file,
        } => commands::run_challenge(
            &dir,
            ChallengeParams {
                id,
                evidence,
                source_file,
            },
            &formatter,
        ),
        Command::Gc { confirm, threshold } => {
            commands::run_gc(&dir, confirm, threshold, &formatter)
        }
        Command::Compress {
            scope,
            threshold,
            confirm,
        } => commands::run_compress(&dir, scope, threshold, confirm, &formatter),
        Command::Serve { transport, port } => {
            commands::run_serve(&dir, &transport, port, &formatter)
        }
        Command::Completions { shell } => {
            commands::run_completions(shell);
            Ok(())
        }
        Command::Reindex {
            embeddings_only,
            index_only,
        } => commands::run_reindex(&dir, embeddings_only, index_only, &formatter),
        Command::Review {
            scope,
            type_,
            challenged_only,
            stale_only,
        } => commands::run_review(&dir, scope, type_, challenged_only, stale_only, &formatter),
    }
}
