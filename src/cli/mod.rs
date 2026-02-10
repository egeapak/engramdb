pub mod app;
pub mod output;
pub mod commands;

use anyhow::Result;
use std::env;
use std::path::PathBuf;

use app::{Cli, Command};
use output::OutputFormatter;

pub fn run(cli: Cli) -> Result<()> {
    // Determine working directory
    let dir = cli.dir.unwrap_or_else(|| {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    });

    // Create output formatter
    let formatter = OutputFormatter::new(cli.format, cli.json, cli.no_color);

    // Dispatch to command handlers
    match cli.command {
        Command::Init => {
            commands::run_init(&dir, &formatter)
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
        } => {
            commands::run_add(
                &dir,
                &type_,
                &content,
                summary,
                physical,
                logical,
                tags,
                criticality,
                confidence,
                details,
                &visibility,
                &formatter,
            )
        }
        Command::Get { id } => {
            commands::run_get(&dir, &id, &formatter)
        }
        Command::Retrieve {
            path,
            logical,
            query,
            type_,
            tags,
            min_criticality,
            max_results,
        } => {
            commands::run_retrieve(
                &dir,
                path,
                logical,
                query,
                type_,
                tags,
                min_criticality,
                max_results,
                &formatter,
            )
        }
        Command::Search {
            query,
            type_,
            tags,
            physical,
            logical,
            min_criticality,
        } => {
            commands::run_search(
                &dir,
                &query,
                type_,
                tags,
                physical,
                logical,
                min_criticality,
                &formatter,
            )
        }
        Command::List {
            type_,
            tags,
            status,
        } => {
            commands::run_list(&dir, type_, tags, status, &formatter)
        }
        Command::Update {
            id,
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
            status,
        } => {
            commands::run_update(
                &dir,
                &id,
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
                status,
                &formatter,
            )
        }
        Command::Delete { id, force } => {
            commands::run_delete(&dir, &id, force, &formatter)
        }
        Command::Stats => {
            commands::run_stats(&dir, &formatter)
        }
    }
}
