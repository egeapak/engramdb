//! Compress command — lists candidates, directs users to MCP mode for actual compression.

use crate::output::{short_id, OutputFormatter};
use crate::validation::validate_score;
use anyhow::Result;
use engramdb::ops;
use engramdb::storage::MemoryStore;
use std::path::Path;

/// List compression candidates and direct users to MCP mode.
pub async fn run_compress(
    dir: &Path,
    global: bool,
    scope: Option<String>,
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
    let result = ops::compress_candidates(&store, scope.as_deref(), threshold).await?;

    if result.candidates.is_empty() {
        formatter.print_message("No compression candidates found.");
        return Ok(());
    }

    formatter.print_message(&format!(
        "Compression candidates ({} memories, threshold {:.2}):\n",
        result.total, result.threshold
    ));

    for candidate in &result.candidates {
        let id_short = short_id(&candidate.id);
        println!(
            "  {} {:8}  {} (criticality: {:.2})",
            id_short, candidate.type_, candidate.summary, candidate.criticality
        );
    }

    formatter.print_message(
        "\nCompression requires an LLM agent. Use MCP mode (engramdb serve) with a connected agent.",
    );

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
    async fn test_compress_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);

        let result = run_compress(temp_dir.path(), false, None, None, &formatter).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_compress_with_low_criticality_memories() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let result = run_compress(temp_dir.path(), false, None, None, &formatter).await;
        assert!(result.is_ok());
    }
}
