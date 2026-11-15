//! Garbage collection command.

use crate::output::{outln, short_id, OutputFormatter};
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

    // JSON mode: emit a single parseable object (the human flow below mixes
    // print_* JSON with raw per-id println! lines, which corrupts the stream for
    // scripted consumers — finding #7). The dry-run plan is exactly what a
    // script wants to parse.
    if formatter.is_json() {
        // Snapshot candidate summaries before any deletion: on a real run the
        // memories are gone by render time, so a post-hoc per-id store.get
        // always failed and degraded every entry to a bare {"id"} (finding
        // #5). One batched load replaces N lookups. gc_memories re-plans and
        // re-validates under the write lock, so the snapshot is display-only;
        // an id that races past it falls back to the bare form.
        let plan = engramdb::ops::plan_gc(&store, &config, threshold).await?;
        let candidate_ids: Vec<String> = plan.candidates.iter().map(|c| c.id.clone()).collect();
        let mut summaries: std::collections::HashMap<String, serde_json::Value> = store
            .get_batch(&candidate_ids)
            .await?
            .into_iter()
            .map(|(id, m)| {
                let entry = serde_json::json!({
                    "id": id.as_str(),
                    "type": format!("{:?}", m.type_),
                    "summary": m.summary,
                    "criticality": m.criticality,
                });
                (id, entry)
            })
            .collect();

        let result = gc_memories(&store, &config, dry_run, threshold).await?;
        let removed: Vec<serde_json::Value> = result
            .removed
            .iter()
            .map(|id| {
                summaries
                    .remove(id)
                    .unwrap_or_else(|| serde_json::json!({ "id": id }))
            })
            .collect();
        let maintenance = result.maintenance.as_ref().map(|m| {
            serde_json::json!({
                "bytes_removed": m.bytes_removed,
                "old_versions_removed": m.old_versions_removed,
            })
        });
        outln!(
            formatter,
            "{}",
            serde_json::json!({
                "dry_run": dry_run,
                "count": result.count,
                "removed": removed,
                "skipped": result.skipped,
                "stale_entries": result.stale_entries.len(),
                "unparseable": result.unparseable,
                "maintenance": maintenance,
            })
        );
        return Ok(());
    }

    let result = gc_memories(&store, &config, dry_run, threshold).await?;

    if !result.stale_entries.is_empty() {
        formatter.print_warning(&format!(
            "Found {} stale index entries (missing data). Run `engramdb reindex` to fix.",
            result.stale_entries.len()
        ));
    }

    if !result.unparseable.is_empty() {
        formatter.print_warning(&format!(
            "{} memory file{} do{} not parse (see `engramdb doctor`).",
            result.unparseable.len(),
            if result.unparseable.len() == 1 {
                ""
            } else {
                "s"
            },
            if result.unparseable.len() == 1 {
                "es"
            } else {
                ""
            },
        ));
    }

    if result.count == 0 {
        formatter.print_message("No memories eligible for garbage collection.");
    } else if dry_run {
        formatter.print_message(&format!(
            "Found {} memories eligible for removal (dry run):",
            result.count
        ));
        // One batched load, matching the JSON branch above (finding #5): a
        // dry run lists every candidate, and each `get` is a full directory
        // scan, so the pretty branch was the one still paying O(N^2).
        let removed_refs: Vec<&str> = result.removed.iter().map(String::as_str).collect();
        let loaded: std::collections::HashMap<String, engramdb::types::Memory> = store
            .get_batch(&removed_refs)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        for id in &result.removed {
            let id_short = short_id(id);
            match loaded.get(id).ok_or(()) {
                Ok(memory) => {
                    outln!(
                        formatter,
                        "  {} {:8}  {} (criticality: {:.2})",
                        id_short,
                        format!("{:?}", memory.type_),
                        memory.summary,
                        memory.criticality
                    );
                }
                Err(()) => {
                    outln!(formatter, "  {}", id_short);
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
    use engramdb::storage::{InMemoryRegistry, MemoryStore};
    use engramdb::types::{Memory, MemoryType, Provenance, Status, Visibility};
    use tempfile::TempDir;

    fn create_test_memory(id: &str, type_: MemoryType, criticality: f64) -> Memory {
        Memory {
            id: id.to_string(),
            type_,
            epistemic: type_.default_epistemic(),
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
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
            audience: None,
            source_sessions: vec![],
            challenges: vec![],
            links: vec![],
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

        let (formatter, cap) = crate::testutil::capturing_plain();

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;

        result.unwrap();
        assert_eq!(
            cap.stdout(),
            "No memories eligible for garbage collection.\n"
        );
    }

    #[tokio::test]
    async fn test_gc_dry_run_with_memories() {
        let (temp_dir, store) = setup_test_store().await;
        let (formatter, cap) = crate::testutil::capturing_plain();

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;
        result.unwrap();
        assert_eq!(
            cap.stdout(),
            "No memories eligible for garbage collection.\n"
        );
        assert_eq!(
            store.list_ids().await.unwrap().len(),
            3,
            "a dry run deletes nothing"
        );
    }

    #[tokio::test]
    async fn test_gc_dry_run_json_mode() {
        let (temp_dir, _store) = setup_test_store().await;
        let (formatter, cap) = crate::testutil::capturing_json();

        let result = run_gc(temp_dir.path(), false, false, None, &formatter).await;
        result.unwrap();
        let v: serde_json::Value = serde_json::from_str(cap.stdout().trim()).unwrap();
        assert_eq!(v["dry_run"], true, "{v}");
        assert_eq!(v["count"], 0, "{v}");
        assert_eq!(v["removed"], serde_json::json!([]), "{v}");
    }

    #[tokio::test]
    async fn test_gc_confirm_json_mode() {
        // Exercises the pre-deletion summary snapshot (finding #5): summaries
        // are batched before gc_memories deletes, so the JSON path must not
        // depend on post-deletion lookups.
        let (temp_dir, store) = setup_test_store().await;
        let (formatter, cap) = crate::testutil::capturing_json();

        let result = run_gc(temp_dir.path(), false, true, Some(1.0), &formatter).await;
        result.unwrap();
        let v: serde_json::Value = serde_json::from_str(cap.stdout().trim()).unwrap();
        assert_eq!(v["dry_run"], false, "{v}");
        assert_eq!(v["count"], 3, "{v}");
        // The summaries were captured before the deletes.
        let removed: Vec<&str> = v["removed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["summary"].as_str().unwrap())
            .collect();
        assert_eq!(
            removed,
            [
                "Test summary for mem-test-001-aaa",
                "Test summary for mem-test-002-bbb",
                "Test summary for mem-test-003-ccc"
            ]
        );

        // Threshold 1.0 collects everything scoring below the clamp ceiling.
        let remaining = store.list_ids().await.unwrap();
        assert!(remaining.len() < 3, "gc --confirm should delete memories");
    }

    /// A memory file that does not parse is reported as such, not as a stale
    /// index entry (reindex would keep its row).
    #[tokio::test]
    async fn snap_gc_reports_an_unparseable_file() {
        use crate::testutil::{capturing_plain, snap_command, TempProject};
        let p = TempProject::new();
        let store = p.init_store().await;
        let mut mem = create_test_memory("mem-test-broken-01", MemoryType::Decision, 0.9);
        mem.supersedes = vec!["old-1".into()];
        store.create(&mem).await.unwrap();
        let path = p
            .path()
            .join(".engramdb/memories")
            .join("mem-test-broken-01.md");
        let clean = std::fs::read_to_string(&path).unwrap();
        let conflicted = clean.replace(
            "supersedes:\n- old-1\n",
            "<<<<<<< HEAD\nsupersedes:\n- old-1\n=======\nsupersedes: []\n>>>>>>> topic\n",
        );
        assert_ne!(clean, conflicted);
        std::fs::write(&path, conflicted).unwrap();

        let (formatter, cap) = capturing_plain();
        run_gc(p.path(), false, false, None, &formatter)
            .await
            .unwrap();
        snap_command("gc_reports_an_unparseable_file", p.path(), cap.transcript());
    }
}
