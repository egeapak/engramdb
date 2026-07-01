//! Rollback memory files to a previous format version.

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::storage::memory_file::{
    detect_format_version, parser_for_version, writer_for_version, CURRENT_FORMAT_VERSION,
};
use engramdb::storage::paths;
use std::path::Path;

/// Resolve a user-supplied `--target-version` to the internal representation
/// (`1` ⇒ `None`, the legacy no-version format), rejecting versions outside the
/// supported range instead of silently writing the wrong format.
///
/// Before this, `target_version <= 1` became `None` (so `0` silently rolled to
/// v1) and any larger unknown version (e.g. `5`) fell through to the V1 writer
/// while messages claimed "v5" (finding #22).
pub(crate) fn resolve_rollback_target(target_version: u32) -> Result<Option<u32>> {
    match target_version {
        0 => anyhow::bail!(
            "Invalid rollback target version 0; valid versions are 1..={CURRENT_FORMAT_VERSION}"
        ),
        1 => Ok(None),
        v if v <= CURRENT_FORMAT_VERSION => Ok(Some(v)),
        v => anyhow::bail!(
            "Unsupported rollback target version {v}; valid versions are 1..={CURRENT_FORMAT_VERSION}"
        ),
    }
}

/// Run the rollback command.
///
/// Scans all memory files (shared and personal) and rewrites them using the
/// specified target format version.
///
/// With `--dry-run`, only reports what would be rolled back without changing files.
pub async fn run_rollback(
    dir: &Path,
    global: bool,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use engramdb::storage::memory_file::{parser_for_version, MemoryWriter as _, V2Writer};
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn json_formatter() -> OutputFormatter {
        OutputFormatter::new(Some(OutputFormat::Json), false, false)
    }

    // Finding #22: rollback target-version resolution rejects unsupported
    // versions instead of silently writing the wrong format.
    #[test]
    fn resolve_rollback_target_validates() {
        // POSITIVE: 1 → None (legacy), valid versions → Some.
        assert_eq!(resolve_rollback_target(1).unwrap(), None);
        assert_eq!(
            resolve_rollback_target(CURRENT_FORMAT_VERSION).unwrap(),
            Some(CURRENT_FORMAT_VERSION)
        );
        // NEGATIVE (red before fix): 0 and out-of-range versions are errors,
        // not a silent fall-through to the v1 writer.
        assert!(resolve_rollback_target(0).is_err());
        assert!(resolve_rollback_target(CURRENT_FORMAT_VERSION + 1).is_err());
    }

    fn engramdb_layout(root: &std::path::Path) -> std::path::PathBuf {
        let engramdb = root.join(".engramdb");
        std::fs::create_dir_all(engramdb.join("memories")).unwrap();
        // No manifest.toml: matches migrate.rs test layout and exercises only
        // the shared-memories rewrite path.
        engramdb.join("memories")
    }

    fn make_memory() -> Memory {
        Memory::new(
            MemoryType::Convention,
            "Rollback fixture summary",
            "Body content preserved across rollback",
            Provenance::human(),
        )
    }

    #[tokio::test]
    async fn rollback_no_engramdb_dir_is_noop() {
        let tmp = TempDir::new().unwrap();
        run_rollback(tmp.path(), false, None, false, &json_formatter())
            .await
            .unwrap();
        assert!(!tmp.path().join(".engramdb").exists());
    }

    /// CRITICAL guard at rollback.rs:32-37: rolling back *to* the current
    /// version must be rejected (use `migrate` instead). Without this guard,
    /// users could "rollback" a v2 file to v2 — a no-op masquerading as work
    /// — and the message would lie.
    #[tokio::test]
    async fn rollback_to_current_version_is_rejected_and_does_not_touch_files() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        // A v2 file we'd expect to be left untouched.
        let mem = make_memory();
        let content = V2Writer.write(&mem).unwrap();
        let file = memories_dir.join(format!("{}.md", mem.id));
        std::fs::write(&file, &content).unwrap();

        run_rollback(
            tmp.path(),
            false,
            Some(CURRENT_FORMAT_VERSION),
            false,
            &json_formatter(),
        )
        .await
        .unwrap();

        // Guard kicked in: file is byte-identical.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
    }

    #[tokio::test]
    async fn rollback_dry_run_does_not_modify_files() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        let mem = make_memory();
        let content = V2Writer.write(&mem).unwrap();
        let file = memories_dir.join(format!("{}.md", mem.id));
        std::fs::write(&file, &content).unwrap();

        // Roll back to v1 (None) in dry-run.
        run_rollback(tmp.path(), false, None, true, &json_formatter())
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
    }

    #[tokio::test]
    async fn rollback_v2_to_v1_round_trips() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        let original = make_memory();
        let v2_content = V2Writer.write(&original).unwrap();
        let file = memories_dir.join(format!("{}.md", original.id));
        std::fs::write(&file, &v2_content).unwrap();

        run_rollback(tmp.path(), false, None, false, &json_formatter())
            .await
            .unwrap();

        let after = std::fs::read_to_string(&file).unwrap();
        // Version stripped: detect returns None for legacy v1.
        assert_eq!(detect_format_version(&after), None);

        // Re-parse with v1 parser and verify Memory data survived.
        let reparsed = parser_for_version(None).parse(&after).unwrap();
        assert_eq!(reparsed.id, original.id);
        assert_eq!(reparsed.type_, original.type_);
        assert_eq!(reparsed.summary, original.summary);
        assert_eq!(reparsed.content, original.content);
    }

    #[tokio::test]
    async fn rollback_already_at_target_is_noop() {
        let tmp = TempDir::new().unwrap();
        let memories_dir = engramdb_layout(tmp.path());

        // A v1 file with target = None: already at target, nothing to do.
        let mem = make_memory();
        let v1 = engramdb::storage::memory_file::V1Writer
            .write(&mem)
            .unwrap();
        let file = memories_dir.join(format!("{}.md", mem.id));
        std::fs::write(&file, &v1).unwrap();

        run_rollback(tmp.path(), false, None, false, &json_formatter())
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), v1);
    }
}
