//! Garbage collection command.

use crate::output::{short_id, OutputFormatter};
use crate::validation::validate_score;
use anyhow::Result;
use engramdb::ops::gc_memories;
use engramdb::storage::MemoryStore;
use std::path::Path;

/// Run garbage collection.
///
/// Default mode is dry-run (shows what would be deleted).
/// Use --confirm to actually delete.
pub async fn run_gc(
    dir: &Path,
    global: bool,
    confirm: bool,
    threshold: Option<f64>,
    formatter: &OutputFormatter,
) -> Result<()> {
    if let Some(t) = threshold {
        validate_score(t, "threshold")?;
    }

    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    let config_path = store.project_dir.join(".engramdb").join("config.toml");
    let config = engramdb::storage::config::load_config(&config_path).await?;

    let dry_run = !confirm;
    let result = gc_memories(&store, &config, dry_run, threshold).await?;

    // JSON mode: emit a single parseable object (the human flow below mixes
    // print_* JSON with raw per-id println! lines, which corrupts the stream for
    // scripted consumers — finding #7). The dry-run plan is exactly what a
    // script wants to parse.
    if formatter.is_json() {
        let mut removed = Vec::with_capacity(result.removed.len());
        for id in &result.removed {
            match store.get(id).await {
                Ok(m) => removed.push(serde_json::json!({
                    "id": id,
                    "type": format!("{:?}", m.type_),
                    "summary": m.summary,
                    "criticality": m.criticality,
                })),
                Err(_) => removed.push(serde_json::json!({ "id": id })),
            }
        }
        let maintenance = result.maintenance.as_ref().map(|m| {
            serde_json::json!({
                "bytes_removed": m.bytes_removed,
                "old_versions_removed": m.old_versions_removed,
            })
        });
        println!(
            "{}",
            serde_json::json!({
                "dry_run": dry_run,
                "count": result.count,
                "removed": removed,
                "skipped": result.skipped,
                "stale_entries": result.stale_entries.len(),
                "maintenance": maintenance,
            })
        );
        return Ok(());
    }

    if !result.stale_entries.is_empty() {
        formatter.print_warning(&format!(
            "Found {} stale index entries (missing data). Run `engramdb reindex` to fix.",
            result.stale_entries.len()
        ));
    }

    if result.count == 0 {
        formatter.print_message("No memories eligible for garbage collection.");
    } else if dry_run {
        formatter.print_message(&format!(
            "Found {} memories eligible for removal (dry run):",
            result.count
        ));
        for id in &result.removed {
            let id_short = short_id(id);
            match store.get(id).await {
                Ok(memory) => {
                    println!(
                        "  {} {:8}  {} (criticality: {:.2})",
                        id_short,
                        format!("{:?}", memory.type_),
                        memory.summary,
                        memory.criticality
                    );
                }
                Err(_) => {
                    println!("  {}", id_short);
                }
            }
        }
        formatter.print_message("\nRun with --confirm to delete these memories.");
    } else {
        formatter.print_success(&format!("Removed {} memories.", result.count));
        if let Some(m) = &result.maintenance {
            if m.bytes_removed > 0 {
                formatter.print_message(&format!(
                    "Index maintenance reclaimed {} bytes ({} old index versions pruned).",
                    m.bytes_removed, m.old_versions_removed
                ));
            }
        }
        if !result.skipped.is_empty() {
            formatter.print_warning(&format!(
                "Skipped {} candidate(s) that changed or were deleted concurrently: {}",
                result.skipped.len(),
                result
                    .skipped
                    .iter()
                    .map(|id| short_id(id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputFormatter;
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::types::{Memory, MemoryType, Provenance, Status, Visibility};
    use tempfile::TempDir;

    fn create_test_memory(id: &str, type_: MemoryType, criticality: f64) -> Memory {
        Memory {
            id: id.to_string(),
            type_,
            summary: format!("Test summary for {}", id),
            title: None,
            content: format!("Test content for {}", id),
            details: None,
            physical: vec!["src/main.rs".to_string()],
            logical: vec!["app.core".to_string()],
            tags: vec![],
            criticality,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.9,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            accessed_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    async fn setup_test_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mem1 = create_test_memory("mem-test-001-aaa", MemoryType::Decision, 0.9);
        let mem2 = create_test_memory("mem-test-002-bbb", MemoryType::Hazard, 0.7);
        let mem3 = create_test_memory("mem-test-003-ccc", MemoryType::Convention, 0.3);

        store.create(&mem1).await.unwrap();
        store.create(&mem2).await.unwrap();
        store.create(&mem3).await.unwrap();

        (temp_dir, store)
    }

    #[tokio::test]
    async fn test_gc_dry_run_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_gc_dry_run_with_memories() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;
        assert!(result.is_ok());
    }
}
