//! Memory compression operations.
//!
//! Provides two functions:
//! - `compress_candidates` — lists memories eligible for compression
//! - `compress_apply` — creates a summary memory that supersedes the given sources

use crate::ops::{create_memory, CreateParams};
use crate::storage::MemoryStore;
use crate::title::TitleStrategy;
use crate::types::{MemoryType, Provenance, Visibility};
use anyhow::{bail, Result};
use serde::Serialize;

/// A memory eligible for compression.
#[derive(Debug, Clone, Serialize)]
pub struct CompressCandidate {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub summary: String,
    pub criticality: f64,
}

/// Result of listing compression candidates.
#[derive(Debug, Serialize)]
pub struct CompressCandidatesResult {
    pub candidates: Vec<CompressCandidate>,
    pub total: usize,
    pub threshold: f64,
}

/// Result of applying compression.
#[derive(Debug, Serialize)]
pub struct CompressApplyResult {
    pub new_id: String,
    /// Number of source memories the summary supersedes (always
    /// `source_ids.len()` — the `supersedes` list on the new memory).
    pub superseded_count: usize,
    /// Source IDs that were already gone when deletion ran (deleted
    /// concurrently after validation). The summary memory is still valid;
    /// its `supersedes` may reference these missing IDs, which is harmless —
    /// `supersedes` is informational metadata and is never dereferenced.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_sources: Vec<String>,
}

/// List memories eligible for compression based on criticality threshold and scope.
pub async fn compress_candidates(
    store: &MemoryStore,
    scope: Option<&str>,
    threshold: Option<f64>,
) -> Result<CompressCandidatesResult> {
    let entries = store.list_filterable().await?;
    let threshold = threshold.unwrap_or(0.4);

    let candidates: Vec<CompressCandidate> = entries
        .iter()
        .filter(|e| {
            if let Some(scope) = scope {
                e.logical.iter().any(|s| s == scope) || e.physical.iter().any(|p| p == scope)
            } else {
                true
            }
        })
        .filter(|e| e.criticality <= threshold)
        .map(|e| CompressCandidate {
            id: e.id.clone(),
            type_: format!("{:?}", e.type_).to_lowercase(),
            summary: e.summary.clone(),
            criticality: e.criticality,
        })
        .collect();

    let total = candidates.len();
    Ok(CompressCandidatesResult {
        candidates,
        total,
        threshold,
    })
}

/// Create a summary memory that supersedes the given source memories.
///
/// The new memory is created as type Context with provenance agent("compress").
/// The caller (typically an LLM agent) provides the summary and content.
pub async fn compress_apply(
    store: &MemoryStore,
    source_ids: Vec<String>,
    summary: String,
    content: String,
    scope: Option<Vec<String>>,
    tags: Option<Vec<String>>,
) -> Result<CompressApplyResult> {
    if source_ids.is_empty() {
        bail!("source_ids must not be empty");
    }

    // Validate all source IDs exist (single dir scan, no file reads),
    // immediately before creating the summary. This cannot be transactional
    // (no cross-file transactions exist), but keeping the check adjacent to
    // the create shrinks the window in which a source can vanish unnoticed.
    let existing = store
        .batch_exists(&source_ids)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to check source IDs: {}", e))?;
    for id in &source_ids {
        if !existing.contains(id.as_str()) {
            bail!("Source memory not found: {}", id);
        }
    }

    let superseded_count = source_ids.len();

    let result = create_memory(
        store,
        CreateParams {
            type_: MemoryType::Context,
            content,
            summary,
            title: None,
            physical: vec!["/".to_string()],
            logical: scope.unwrap_or_default(),
            tags: tags.unwrap_or_default(),
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Shared,
            provenance: Provenance::agent("compress"),
            supersedes: source_ids.clone(),
            epistemic: None,
            premise: None,
            invalidated_by: vec![],
            origin_task: None,
            generality: None,
            valid_from: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title_strategy: TitleStrategy::None,
            embed_async: false,
        },
        None,
    )
    .await?;

    // Sources are INVALIDATED, not deleted (§2.4 writer 3): `create_memory`
    // already closed each live source's validity window (`invalidated_at =
    // now`, `superseded_by = <summary id>`) via its supersession pass. The
    // files stay on disk — queryable under `include_invalidated`, purged
    // eventually by gc's retention rule. Here we verify the outcome so
    // partial failures surface exactly like the old delete loop did:
    // - a source that vanished concurrently is skipped and reported in
    //   `skipped_sources`;
    // - a source still live (its window-close failed, e.g. I/O) is recorded,
    //   the REMAINING sources are still checked, and a partial-failure error
    //   listing the un-invalidated IDs (and the new memory's ID) is returned
    //   so the user can re-run. The summary memory remains valid either way.
    let mut skipped_sources = Vec::new();
    let mut failed_sources: Vec<String> = Vec::new();
    for id in &source_ids {
        match store.get(id).await {
            Err(crate::storage::StorageError::NotFound(_)) => {
                skipped_sources.push(id.clone());
            }
            Err(e) => failed_sources.push(format!("{} ({})", id, e)),
            // Invalidated — by this compress or an earlier writer; either
            // way the window is closed.
            Ok(m) if m.is_invalidated() => {}
            Ok(_) => failed_sources.push(format!("{} (still active)", id)),
        }
    }

    if !failed_sources.is_empty() {
        bail!(
            "Compressed memory {} was created, but {} source memor{} could not be invalidated: {}. \
             Re-run compress or `resolve --action invalidate` the listed memories manually \
             (the compressed memory is valid and supersedes them).",
            result.id,
            failed_sources.len(),
            if failed_sources.len() == 1 { "y" } else { "ies" },
            failed_sources.join(", ")
        );
    }

    Ok(CompressApplyResult {
        new_id: result.id,
        superseded_count,
        skipped_sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance, ProvenanceSource, Visibility};
    use tempfile::TempDir;

    async fn setup_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        (temp_dir, store)
    }

    async fn add_memory(
        store: &MemoryStore,
        type_: MemoryType,
        summary: &str,
        criticality: f64,
        logical: Vec<String>,
    ) -> String {
        let result = create_memory(
            store,
            CreateParams {
                type_,
                content: format!("Content for {}", summary),
                summary: summary.to_string(),
                title: None,
                physical: vec!["/".to_string()],
                logical,
                tags: vec![],
                criticality,
                confidence: 0.8,
                details: None,
                visibility: Visibility::Shared,
                provenance: Provenance::human(),
                supersedes: vec![],
                epistemic: None,
                premise: None,
                invalidated_by: vec![],
                origin_task: None,
                generality: None,
                valid_from: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                title_strategy: TitleStrategy::None,
                embed_async: false,
            },
            None,
        )
        .await
        .unwrap();
        result.id
    }

    #[tokio::test]
    async fn test_compress_candidates_basic() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Debug, "low crit debug", 0.1, vec![]).await;
        add_memory(
            &store,
            MemoryType::Decision,
            "high crit decision",
            0.9,
            vec![],
        )
        .await;
        add_memory(
            &store,
            MemoryType::Context,
            "medium crit context",
            0.3,
            vec![],
        )
        .await;

        let result = compress_candidates(&store, None, Some(0.4)).await.unwrap();

        assert_eq!(result.total, 2);
        assert_eq!(result.threshold, 0.4);
        let summaries: Vec<&str> = result
            .candidates
            .iter()
            .map(|c| c.summary.as_str())
            .collect();
        assert!(summaries.contains(&"low crit debug"));
        assert!(summaries.contains(&"medium crit context"));
        assert!(!summaries.contains(&"high crit decision"));
    }

    #[tokio::test]
    async fn test_compress_candidates_scope_filter() {
        let (_temp, store) = setup_store().await;

        add_memory(
            &store,
            MemoryType::Debug,
            "auth debug",
            0.1,
            vec!["auth".to_string()],
        )
        .await;
        add_memory(
            &store,
            MemoryType::Debug,
            "db debug",
            0.1,
            vec!["db".to_string()],
        )
        .await;

        let result = compress_candidates(&store, Some("auth"), Some(0.4))
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.candidates[0].summary, "auth debug");
    }

    #[tokio::test]
    async fn test_compress_candidates_empty() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Decision, "important", 0.9, vec![]).await;

        let result = compress_candidates(&store, None, Some(0.4)).await.unwrap();
        assert_eq!(result.total, 0);
        assert!(result.candidates.is_empty());
    }

    #[tokio::test]
    async fn test_compress_apply_basic() {
        let (_temp, store) = setup_store().await;

        let id1 = add_memory(&store, MemoryType::Debug, "debug 1", 0.1, vec![]).await;
        let id2 = add_memory(&store, MemoryType::Debug, "debug 2", 0.2, vec![]).await;

        let result = compress_apply(
            &store,
            vec![id1.clone(), id2.clone()],
            "Combined debug summary".to_string(),
            "Merged content from debug 1 and 2".to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.superseded_count, 2);
        assert!(
            result.skipped_sources.is_empty(),
            "all sources existed, nothing should be skipped"
        );

        // Verify the new memory exists and has correct supersedes
        let new_memory = store.get(&result.new_id).await.unwrap();
        assert_eq!(new_memory.type_, MemoryType::Context);
        assert_eq!(new_memory.summary, "Combined debug summary");
        assert!(new_memory.supersedes.contains(&id1));
        assert!(new_memory.supersedes.contains(&id2));

        // Both sources survive on disk with CLOSED validity windows (§2.4
        // writer 3) — invalidated, superseded by the summary, not deleted.
        for id in [&id1, &id2] {
            let source = store.get(id).await.unwrap();
            assert!(source.invalidated_at.is_some(), "window must be closed");
            assert_eq!(
                source.superseded_by.as_deref(),
                Some(result.new_id.as_str())
            );
        }
    }

    /// A source whose index row is missing (half-deleted by a crash) is
    /// still invalidated through its on-disk file — the window-closing pass
    /// operates on files, so nothing is skipped and the summary stays valid.
    #[tokio::test]
    async fn test_compress_apply_source_without_index_row_still_invalidated() {
        let (temp, store) = setup_store().await;

        let real_id = add_memory(&store, MemoryType::Debug, "real source", 0.1, vec![]).await;

        let ghost = Memory::new(
            MemoryType::Debug,
            "ghost source",
            "gone before deletion",
            Provenance::human(),
        );
        let ghost_id = ghost.id.clone();
        let content = crate::storage::memory_file::write_memory_file(&ghost).unwrap();
        let memories_dir = temp.path().join(".engramdb").join("memories");
        std::fs::write(memories_dir.join(format!("{}.md", ghost_id)), content).unwrap();

        let result = compress_apply(
            &store,
            vec![real_id.clone(), ghost_id.clone()],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.superseded_count, 2);
        assert!(
            result.skipped_sources.is_empty(),
            "a file-backed source is invalidatable even without an index row"
        );

        let new_memory = store.get(&result.new_id).await.unwrap();
        assert!(new_memory.supersedes.contains(&real_id));
        assert!(new_memory.supersedes.contains(&ghost_id));

        // Both sources were invalidated in place, not deleted.
        for id in [&real_id, &ghost_id] {
            let source = store.get(id).await.unwrap();
            assert!(source.invalidated_at.is_some());
        }
    }

    /// A REAL invalidation error (I/O) must not abort the sweep mid-way:
    /// the remaining sources are still processed, and the returned error
    /// lists the still-active IDs plus the (valid) new memory's ID.
    ///
    /// Failure injection: the first source's `.md` file is replaced by a
    /// directory of the same name — reading/rewriting it fails with a
    /// genuine I/O error rather than NotFound, and it works regardless of
    /// the user the tests run as (unlike chmod tricks, which root ignores).
    #[tokio::test]
    async fn test_compress_apply_continues_past_real_invalidate_failure() {
        let (temp, store) = setup_store().await;

        let broken_id = add_memory(&store, MemoryType::Debug, "undeletable", 0.1, vec![]).await;
        let ok_id = add_memory(&store, MemoryType::Debug, "deletable", 0.1, vec![]).await;

        // Replace broken's file with a same-named directory.
        let memories_dir = temp.path().join(".engramdb").join("memories");
        for entry in std::fs::read_dir(&memories_dir).unwrap() {
            let path = entry.unwrap().path();
            let is_broken = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.contains(&broken_id))
                .unwrap_or(false);
            if is_broken {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
        }

        // broken first, ok second: proves the loop continues past the failure.
        let err = compress_apply(
            &store,
            vec![broken_id.clone(), ok_id.clone()],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("could not be invalidated"),
            "partial failure must be reported: {}",
            msg
        );
        assert!(
            msg.contains(&broken_id),
            "error must list the still-active id: {}",
            msg
        );
        assert!(
            !msg.contains(&ok_id),
            "successfully invalidated source must not be listed as failed: {}",
            msg
        );

        // The later source was still processed and invalidated.
        assert!(store.get(&ok_id).await.unwrap().invalidated_at.is_some());

        // The summary memory was created and remains valid.
        let entries = store.list_filterable().await.unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.summary == "Summary" && e.type_ == MemoryType::Context),
            "summary memory must exist despite the partial deletion failure"
        );
    }

    #[tokio::test]
    async fn test_compress_apply_invalid_source() {
        let (_temp, store) = setup_store().await;

        let result = compress_apply(
            &store,
            vec!["nonexistent-id".to_string()],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Source memory not found"));
    }

    #[tokio::test]
    async fn test_compress_candidates_default_threshold_is_0_4() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Debug, "low crit", 0.3, vec![]).await;
        add_memory(&store, MemoryType::Decision, "high crit", 0.9, vec![]).await;

        let result = compress_candidates(&store, None, None).await.unwrap();

        assert_eq!(result.threshold, 0.4);
        assert_eq!(result.total, 1);
    }

    #[tokio::test]
    async fn test_compress_candidates_includes_equal_to_threshold() {
        let (_temp, store) = setup_store().await;

        add_memory(&store, MemoryType::Debug, "below threshold", 0.39, vec![]).await;
        add_memory(&store, MemoryType::Debug, "at threshold", 0.4, vec![]).await;
        add_memory(&store, MemoryType::Debug, "above threshold", 0.41, vec![]).await;

        let result = compress_candidates(&store, None, Some(0.4)).await.unwrap();

        assert_eq!(result.total, 2);
        let summaries: Vec<&str> = result
            .candidates
            .iter()
            .map(|c| c.summary.as_str())
            .collect();
        assert!(summaries.contains(&"below threshold"));
        assert!(summaries.contains(&"at threshold"));
        assert!(!summaries.contains(&"above threshold"));
    }

    #[tokio::test]
    async fn test_compress_candidates_physical_scope_match() {
        let (_temp, store) = setup_store().await;

        // Create memory with specific physical scope using Memory::new directly
        let mut mem_auth = Memory::new(
            MemoryType::Debug,
            "auth debug",
            "Auth content",
            Provenance::human(),
        );
        mem_auth.physical = vec!["/src/auth/".to_string()];
        mem_auth.criticality = 0.1;
        store.create(&mem_auth).await.unwrap();

        let mut mem_db = Memory::new(
            MemoryType::Debug,
            "db debug",
            "DB content",
            Provenance::human(),
        );
        mem_db.physical = vec!["/src/db/".to_string()];
        mem_db.criticality = 0.1;
        store.create(&mem_db).await.unwrap();

        let result = compress_candidates(&store, Some("/src/auth/"), Some(0.4))
            .await
            .unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.candidates[0].summary, "auth debug");
    }

    #[tokio::test]
    async fn test_compress_apply_empty_source_ids_returns_error() {
        let (_temp, store) = setup_store().await;

        let result = compress_apply(
            &store,
            vec![],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
    }

    #[tokio::test]
    async fn test_compress_apply_creates_context_type_with_agent_provenance() {
        let (_temp, store) = setup_store().await;

        let id = add_memory(&store, MemoryType::Debug, "source debug", 0.1, vec![]).await;

        let result = compress_apply(
            &store,
            vec![id],
            "Compressed summary".to_string(),
            "Compressed content".to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        let new_memory = store.get(&result.new_id).await.unwrap();
        assert_eq!(new_memory.type_, MemoryType::Context);
        assert_eq!(new_memory.provenance.source, ProvenanceSource::Agent);
        assert_eq!(new_memory.provenance.agent_id, Some("compress".to_string()));
    }

    #[tokio::test]
    async fn test_compress_apply_with_scope_and_tags() {
        let (_temp, store) = setup_store().await;

        let id = add_memory(&store, MemoryType::Debug, "source", 0.1, vec![]).await;

        let result = compress_apply(
            &store,
            vec![id],
            "Scoped summary".to_string(),
            "Scoped content".to_string(),
            Some(vec!["app.auth".to_string(), "app.core".to_string()]),
            Some(vec!["compressed".to_string(), "auth".to_string()]),
        )
        .await
        .unwrap();

        let new_memory = store.get(&result.new_id).await.unwrap();
        assert_eq!(
            new_memory.logical,
            vec!["app.auth".to_string(), "app.core".to_string()]
        );
        assert_eq!(
            new_memory.tags,
            vec!["compressed".to_string(), "auth".to_string()]
        );
    }

    #[tokio::test]
    async fn test_compress_apply_partial_invalid_source_ids_returns_error() {
        let (_temp, store) = setup_store().await;

        let valid_id = add_memory(&store, MemoryType::Debug, "valid source", 0.1, vec![]).await;
        let count_before = store.count().await.unwrap();

        let result = compress_apply(
            &store,
            vec![valid_id, "nonexistent-id".to_string()],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        )
        .await;

        assert!(result.is_err());
        // Verify no new memory was created
        let count_after = store.count().await.unwrap();
        assert_eq!(count_before, count_after);
    }
}
