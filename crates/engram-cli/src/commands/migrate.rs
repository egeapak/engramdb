//! Migrate memory files to the latest format version.

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::storage::memory_file::{
    detect_format_version, latest_writer, parser_for_version, CURRENT_FORMAT_VERSION,
};
use engramdb::storage::paths;
use std::path::Path;

/// Run the migrate command.
///
/// Scans all memory files (shared and personal), detects those using an older
/// format version, and rewrites them in the current format.
///
/// With `--dry-run`, only reports what would be migrated without changing files.
pub async fn run_migrate(
    dir: &Path,
    global: bool,
    dry_run: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let engramdb_dir = dir.join(".engramdb");
    if !engramdb_dir.exists() {
        formatter.print_error("No .engramdb directory found. Run `engramdb init` first.");
        return Ok(());
    }

    let shared_dir = paths::memories_dir(dir);

    // Personal memories live under the store's project_id. The global store
    // uses GLOBAL_PROJECT_ID (its manifest name is "global", which is *not*
    // the id); projects keep the pre-existing manifest-name resolution.
    let personal_dir = if global {
        paths::personal_memories_dir(paths::GLOBAL_PROJECT_ID).ok()
    } else {
        let manifest_path = engramdb_dir.join("manifest.toml");
        if manifest_path.exists() {
            let manifest = engramdb::storage::manifest::load_manifest(&manifest_path).await?;
            paths::personal_memories_dir(&manifest.project).ok()
        } else {
            None
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use engramdb::storage::memory_file::{
        parse_memory_file, parser_for_version, MemoryWriter as _, V1Writer, CURRENT_FORMAT_VERSION,
    };
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn json_formatter() -> OutputFormatter {
        OutputFormatter::new(Some(OutputFormat::Json), false, false)
    }

    fn engramdb_layout(root: &std::path::Path) -> std::path::PathBuf {
        let engramdb = root.join(".engramdb");
        std::fs::create_dir_all(engramdb.join("memories")).unwrap();
        // Intentionally no manifest.toml: migrate's personal_dir resolution
        // returns None on missing manifest (rollback.rs:47-54 / migrate.rs:37-44)
        // and migrate_dir tolerates a missing personal dir. Tests stay focused
        // on the shared-memories rewrite path.
        engramdb.join("memories")
    }

    fn make_memory() -> Memory {
        Memory::new(
            MemoryType::Decision,
            "A migrate fixture summary",
            "Body content that survives a round trip",
            Provenance::human(),
        )
    }

    #[tokio::test]
    async fn migrate_no_engramdb_dir_is_noop() {
        let tmp = TempDir::new().unwrap();
        // No .engramdb subdir — early return, no error.
        run_migrate(tmp.path(), false, false, &json_formatter())
            .await
            .unwrap();
        assert!(!tmp.path().join(".engramdb").exists());
    }

    #[tokio::test]
    async fn migrate_skips_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());
        let stray = memories_dir.join("readme.txt");
        let original = "hello, not a memory\n";
        std::fs::write(&stray, original).unwrap();

        run_migrate(tmp.path(), false, false, &json_formatter())
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&stray).unwrap(), original);
    }

    #[tokio::test]
    async fn migrate_dry_run_does_not_modify_files() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        // Write a v1 (legacy) fixture
        let mem = make_memory();
        let v1_content = V1Writer.write(&mem).unwrap();
        let file = memories_dir.join(format!("{}.md", mem.id));
        std::fs::write(&file, &v1_content).unwrap();

        run_migrate(tmp.path(), false, true, &json_formatter())
            .await
            .unwrap();

        // Byte-identical after dry-run
        assert_eq!(std::fs::read_to_string(&file).unwrap(), v1_content);
    }

    #[tokio::test]
    async fn migrate_v1_file_is_rewritten_to_current_version() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        let mem = make_memory();
        let v1_content = V1Writer.write(&mem).unwrap();
        let file = memories_dir.join(format!("{}.md", mem.id));
        std::fs::write(&file, &v1_content).unwrap();

        run_migrate(tmp.path(), false, false, &json_formatter())
            .await
            .unwrap();

        let after = std::fs::read_to_string(&file).unwrap();
        let detected = detect_format_version(&after);
        assert_eq!(detected, Some(CURRENT_FORMAT_VERSION));
        assert!(after.contains(&format!("version: {}", CURRENT_FORMAT_VERSION)));
    }

    #[tokio::test]
    async fn migrate_round_trips_memory_data() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        let original = make_memory();
        let v1_content = V1Writer.write(&original).unwrap();
        let file = memories_dir.join(format!("{}.md", original.id));
        std::fs::write(&file, &v1_content).unwrap();

        run_migrate(tmp.path(), false, false, &json_formatter())
            .await
            .unwrap();

        let after = std::fs::read_to_string(&file).unwrap();
        let reparsed = parse_memory_file(&after).unwrap();

        assert_eq!(reparsed.id, original.id);
        assert_eq!(reparsed.type_, original.type_);
        assert_eq!(reparsed.summary, original.summary);
        assert_eq!(reparsed.content, original.content);
    }

    #[tokio::test]
    async fn migrate_already_current_is_noop() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        // Write a current-version (v2) file directly.
        let mem = make_memory();
        let current = latest_writer().write(&mem).unwrap();
        let file = memories_dir.join(format!("{}.md", mem.id));
        std::fs::write(&file, &current).unwrap();

        run_migrate(tmp.path(), false, false, &json_formatter())
            .await
            .unwrap();

        // Byte-identical: nothing to migrate, nothing rewritten.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), current);
    }

    #[tokio::test]
    async fn migrate_unparseable_file_is_reported_not_panic() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        // .md file that's not a valid memory file at all.
        let file = memories_dir.join("garbage.md");
        std::fs::write(&file, "this is not a valid memory file\n").unwrap();

        // Must not panic. The error is collected internally and printed
        // via the formatter; the returned Result is still Ok(()).
        run_migrate(tmp.path(), false, false, &json_formatter())
            .await
            .unwrap();

        // The file should be left as-is on parse failure.
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "this is not a valid memory file\n"
        );
    }

    #[tokio::test]
    async fn migrate_uses_correct_version_parser() {
        // Sanity: a v1 file's detected version is None, and parser_for_version(None)
        // must hand back V1Parser — otherwise migrate would try to parse a v1
        // file with V2Parser and fail. This locks down the dispatch table that
        // migrate_dir relies on.
        let mem = make_memory();
        let v1_content = V1Writer.write(&mem).unwrap();

        assert_eq!(detect_format_version(&v1_content), None);
        let parser = parser_for_version(None);
        let parsed = parser.parse(&v1_content).unwrap();
        assert_eq!(parsed.id, mem.id);
    }
}
