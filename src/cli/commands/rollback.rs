//! Rollback memory files to a previous format version.

use crate::cli::output::OutputFormatter;
use crate::storage::memory_file::{
    detect_format_version, parser_for_version, writer_for_version, CURRENT_FORMAT_VERSION,
};
use crate::storage::paths;
use anyhow::Result;
use std::path::Path;

/// Run the rollback command.
///
/// Scans all memory files (shared and personal) and rewrites them using the
/// specified target format version.
///
/// With `--dry-run`, only reports what would be rolled back without changing files.
pub async fn run_rollback(
    dir: &Path,
    target_version: Option<u32>,
    dry_run: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let engramdb_dir = dir.join(".engramdb");
    if !engramdb_dir.exists() {
        formatter.print_error("No .engramdb directory found. Run `engramdb init` first.");
        return Ok(());
    }

    let target_label = target_version.map_or("v1 (legacy)".to_string(), |v| format!("v{v}"));

    if target_version == Some(CURRENT_FORMAT_VERSION) {
        formatter.print_message(&format!(
            "Target version {target_label} is the current version. Use `engramdb migrate` instead."
        ));
        return Ok(());
    }

    let shared_dir = paths::memories_dir(dir);

    let manifest_path = engramdb_dir.join("manifest.toml");
    let personal_dir = if manifest_path.exists() {
        let manifest = crate::storage::manifest::load_manifest(&manifest_path).await?;
        paths::personal_memories_dir(&manifest.project).ok()
    } else {
        None
    };

    let mut rolled_back = 0u32;
    let mut already_target = 0u32;
    let mut errors = Vec::new();

    rollback_dir(
        &shared_dir,
        target_version,
        dry_run,
        &mut rolled_back,
        &mut already_target,
        &mut errors,
    );

    if let Some(ref pdir) = personal_dir {
        rollback_dir(
            pdir,
            target_version,
            dry_run,
            &mut rolled_back,
            &mut already_target,
            &mut errors,
        );
    }

    if dry_run {
        formatter.print_message(&format!(
            "Dry run: {} memories would be rolled back to {target_label}, {} already at target.",
            rolled_back, already_target
        ));
    } else if rolled_back > 0 {
        formatter.print_success(&format!(
            "Rolled back {} memories to {target_label}. {} were already at target.",
            rolled_back, already_target
        ));
    } else {
        formatter.print_message(&format!(
            "All {} memories are already at {target_label}. Nothing to roll back.",
            already_target
        ));
    }

    if !errors.is_empty() {
        formatter.print_error(&format!("{} errors during rollback:", errors.len()));
        for err in &errors {
            eprintln!("  {err}");
        }
    }

    Ok(())
}

fn rollback_dir(
    dir: &Path,
    target_version: Option<u32>,
    dry_run: bool,
    rolled_back: &mut u32,
    already_target: &mut u32,
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

        let current_version = detect_format_version(&raw);
        if current_version == target_version {
            *already_target += 1;
            continue;
        }

        if dry_run {
            let from = current_version.map_or("legacy".to_string(), |v| format!("v{v}"));
            let to = target_version.map_or("legacy".to_string(), |v| format!("v{v}"));
            eprintln!("  Would roll back: {} ({from} -> {to})", path.display());
            *rolled_back += 1;
            continue;
        }

        // Parse with current version's parser, rewrite with target version's writer
        let parser = parser_for_version(current_version);
        let writer = writer_for_version(target_version);
        match parser.parse(&raw) {
            Ok(memory) => match writer.write(&memory) {
                Ok(new_content) => {
                    if let Err(e) = std::fs::write(&path, &new_content) {
                        errors.push(format!("{}: write error: {e}", path.display()));
                    } else {
                        *rolled_back += 1;
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
