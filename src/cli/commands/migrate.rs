//! Migrate memory files to the latest format version.

use crate::cli::output::OutputFormatter;
use crate::storage::memory_file::{
    detect_format_version, latest_writer, parser_for_version, CURRENT_FORMAT_VERSION,
};
use crate::storage::paths;
use anyhow::Result;
use std::path::Path;

/// Run the migrate command.
///
/// Scans all memory files (shared and personal), detects those using an older
/// format version, and rewrites them in the current format.
///
/// With `--dry-run`, only reports what would be migrated without changing files.
pub async fn run_migrate(dir: &Path, dry_run: bool, formatter: &OutputFormatter) -> Result<()> {
    let engramdb_dir = dir.join(".engramdb");
    if !engramdb_dir.exists() {
        formatter.print_error("No .engramdb directory found. Run `engramdb init` first.");
        return Ok(());
    }

    let shared_dir = paths::memories_dir(dir);

    // Load project_id for personal dir
    let manifest_path = engramdb_dir.join("manifest.toml");
    let personal_dir = if manifest_path.exists() {
        let manifest = crate::storage::manifest::load_manifest(&manifest_path).await?;
        paths::personal_memories_dir(&manifest.project).ok()
    } else {
        None
    };

    let mut migrated = 0u32;
    let mut already_current = 0u32;
    let mut errors = Vec::new();

    // Process shared memories
    migrate_dir(
        &shared_dir,
        dry_run,
        &mut migrated,
        &mut already_current,
        &mut errors,
    )
    .await;

    // Process personal memories
    if let Some(ref pdir) = personal_dir {
        migrate_dir(
            pdir,
            dry_run,
            &mut migrated,
            &mut already_current,
            &mut errors,
        )
        .await;
    }

    // Report results
    if dry_run {
        formatter.print_message(&format!(
            "Dry run: {} memories need migration, {} already at v{}.",
            migrated, already_current, CURRENT_FORMAT_VERSION
        ));
    } else if migrated > 0 {
        formatter.print_success(&format!(
            "Migrated {} memories to v{}. {} were already current.",
            migrated, CURRENT_FORMAT_VERSION, already_current
        ));
    } else {
        formatter.print_message(&format!(
            "All {} memories are already at v{}. Nothing to migrate.",
            already_current, CURRENT_FORMAT_VERSION
        ));
    }

    if !errors.is_empty() {
        formatter.print_error(&format!("{} errors during migration:", errors.len()));
        for err in &errors {
            eprintln!("  {err}");
        }
    }

    Ok(())
}

async fn migrate_dir(
    dir: &Path,
    dry_run: bool,
    migrated: &mut u32,
    already_current: &mut u32,
    errors: &mut Vec<String>,
) {
    if !dir.exists() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(format!("Failed to read {}: {e}", dir.display()));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("Failed to read entry in {}: {e}", dir.display()));
                continue;
            }
        };

        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let raw = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };

        let version = detect_format_version(&raw);
        if version == Some(CURRENT_FORMAT_VERSION) {
            *already_current += 1;
            continue;
        }

        // Needs migration
        if dry_run {
            let from = version.map_or("legacy".to_string(), |v| format!("v{v}"));
            eprintln!(
                "  Would migrate: {} ({from} -> v{CURRENT_FORMAT_VERSION})",
                path.display()
            );
            *migrated += 1;
            continue;
        }

        // Parse with version-specific parser, rewrite with latest writer
        let parser = parser_for_version(version);
        let writer = latest_writer();
        match parser.parse(&raw) {
            Ok(memory) => match writer.write(&memory) {
                Ok(new_content) => {
                    if let Err(e) = std::fs::write(&path, &new_content) {
                        errors.push(format!("{}: write error: {e}", path.display()));
                    } else {
                        *migrated += 1;
                    }
                }
                Err(e) => {
                    errors.push(format!("{}: serialize error: {e}", path.display()));
                }
            },
            Err(e) => {
                errors.push(format!("{}: parse error: {e}", path.display()));
            }
        }
    }
}
