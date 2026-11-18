//! Compress command — lists candidates, directs users to MCP mode for actual compression.

use crate::output::{outln, short_id, OutputFormatter};
use crate::validation::validate_score;
use anyhow::Result;
use engramdb::ops;
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

    let store = super::open_store(dir, global).await?;
    // An explicit `--threshold` wins; otherwise `[thresholds] compress`.
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = engramdb::storage::config::load_config_or_default(&config_path).await;
    let threshold = threshold.or(Some(config.thresholds.compress));
    let result = ops::compress_candidates(&store, scope.as_deref(), threshold).await?;

    // JSON mode: one parseable object (the human flow below prints the candidate
    // list as loose lines, which would corrupt the JSON stream — finding #7).
    if formatter.is_json() {
        let candidates: Vec<_> = result
            .candidates
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "type": c.type_,
                    "summary": c.summary,
                    "criticality": c.criticality,
                })
            })
            .collect();
        outln!(
            formatter,
            "{}",
            serde_json::json!({
                "total": result.total,
                "threshold": result.threshold,
                "candidates": candidates,
            })
        );
        return Ok(());
    }

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
        outln!(
            formatter,
            "  {} {:8}  {} (criticality: {:.2})",
            id_short,
            candidate.type_,
            candidate.summary,
            candidate.criticality
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
    async fn test_compress_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let (formatter, cap) = crate::testutil::capturing_plain();

        let result = run_compress(temp_dir.path(), false, None, None, &formatter).await;

        result.unwrap();
        assert_eq!(cap.stdout(), "No compression candidates found.\n");
    }

    #[tokio::test]
    async fn test_compress_with_low_criticality_memories() {
        let (temp_dir, _store) = setup_test_store().await;
        let (formatter, cap) = crate::testutil::capturing_plain();

        let result = run_compress(temp_dir.path(), false, None, None, &formatter).await;
        result.unwrap();
        let out = cap.stdout();
        // Only the memory under the 0.40 threshold is a candidate.
        assert!(
            out.contains("Compression candidates (1 memories, threshold 0.40)"),
            "{out}"
        );
        assert!(out.contains("Test summary for mem-test-003-ccc"), "{out}");
        assert!(
            !out.contains("mem-test-001") && !out.contains("mem-test-002"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn compress_default_threshold_comes_from_config() {
        let (temp_dir, _store) = setup_test_store().await;
        // mem-test-002 has criticality 0.7: above the 0.4 default, under 0.8.
        std::fs::write(
            temp_dir.path().join(".engramdb").join("config.toml"),
            "[thresholds]\ngc = 0.05\ncompress = 0.8\n",
        )
        .unwrap();
        let (formatter, cap) = crate::testutil::capturing_json();
        run_compress(temp_dir.path(), false, None, None, &formatter)
            .await
            .unwrap();
        let out = cap.stdout();
        assert!(out.contains("mem-test-002-bbb"), "{out}");
        assert!(!out.contains("mem-test-001-aaa"), "{out}");
        assert!(out.contains("0.8"), "{out}");

        // An explicit threshold still wins over the config.
        let (formatter, cap) = crate::testutil::capturing_json();
        run_compress(temp_dir.path(), false, None, Some(0.4), &formatter)
            .await
            .unwrap();
        assert!(!cap.stdout().contains("mem-test-002-bbb"));
    }
}
