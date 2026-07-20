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

/// Parameters for applying compression (mirrors `CreateParams`/`UpdateParams`).
pub struct CompressApplyParams {
    pub source_ids: Vec<String>,
    pub summary: String,
    pub content: String,
    pub scope: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    /// Forwarded to `create_memory` for the replacement summary — see there.
    pub embed_async: bool,
}

/// Create a summary memory that supersedes the given source memories.
///
/// The new memory is created as type Context with provenance agent("compress").
/// The caller (typically an LLM agent) provides the summary and content.
///
/// `engine` embeds the replacement memory: compression invalidates sources
/// that had vectors (default retrieval excludes them), so leaving the
/// consolidated summary UN-embedded would make exactly the compressed
/// knowledge invisible to semantic search until a manual reindex. Pass the
/// same engine the front-end uses for `create`.
pub async fn compress_apply(
    store: &MemoryStore,
    params: CompressApplyParams,
    engine: Option<&crate::retrieval::engine::RetrievalEngine>,
) -> Result<CompressApplyResult> {
    let CompressApplyParams {
        source_ids,
        summary,
        content,
        scope,
        tags,
        embed_async,
    } = params;
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
            embed_async,
        },
        engine,
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
            CompressApplyParams {
                source_ids: vec![id1.clone(), id2.clone()],
                summary: "Combined debug summary".to_string(),
                content: "Merged content from debug 1 and 2".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
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
            CompressApplyParams {
                source_ids: vec![real_id.clone(), ghost_id.clone()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
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
            CompressApplyParams {
                source_ids: vec![broken_id.clone(), ok_id.clone()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
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
            CompressApplyParams {
                source_ids: vec!["nonexistent-id".to_string()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
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
            CompressApplyParams {
                source_ids: vec![],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
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
            CompressApplyParams {
                source_ids: vec![id],
                summary: "Compressed summary".to_string(),
                content: "Compressed content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
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
            CompressApplyParams {
                source_ids: vec![id],
                summary: "Scoped summary".to_string(),
                content: "Scoped content".to_string(),
                scope: Some(vec!["app.auth".to_string(), "app.core".to_string()]),
                tags: Some(vec!["compressed".to_string(), "auth".to_string()]),
                embed_async: false,
            },
            None,
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
            CompressApplyParams {
                source_ids: vec![valid_id, "nonexistent-id".to_string()],
                summary: "Summary".to_string(),
                content: "Content".to_string(),
                scope: None,
                tags: None,
                embed_async: false,
            },
            None,
        )
        .await;

        assert!(result.is_err());
        // Verify no new memory was created
        let count_after = store.count().await.unwrap();
        assert_eq!(count_before, count_after);
    }
}

// ---------------------------------------------------------------------------
// Consolidation (§11.4): observation clusters → derived fact
// ---------------------------------------------------------------------------

/// One consolidation candidate cluster.
#[derive(Debug, Clone)]
pub struct ConsolidationCluster {
    pub source_ids: Vec<String>,
    pub summaries: Vec<String>,
}

/// Report from one consolidation pass.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    /// Candidate clusters (suggestion mode reports these; apply mode also
    /// records what it created).
    pub clusters: Vec<ConsolidationCluster>,
    /// Ids of the Fact memories created (apply mode only).
    pub created: Vec<String>,
    /// True when embedding/NLI providers were unavailable and the pass
    /// skipped (§14.11 graceful-skip contract).
    pub skipped_no_providers: bool,
    /// True when the store had more active observations than one throttled
    /// pass will pairwise-compare (O(n²) bound); nothing was clustered.
    pub skipped_too_many: bool,
}

/// Union-find clustering over similarity pairs. Returns clusters of size ≥
/// `min_size`, each sorted ascending. Pure so the geometry is testable
/// without providers.
pub fn cluster_pairs(n: usize, pairs: &[(usize, usize)], min_size: usize) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut Vec<usize>, x: usize) -> usize {
        if parent[x] != x {
            let root = find(parent, parent[x]);
            parent[x] = root;
        }
        parent[x]
    }
    for &(a, b) in pairs {
        if a >= n || b >= n {
            continue;
        }
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    let mut clusters: Vec<Vec<usize>> = groups
        .into_values()
        .filter(|g| g.len() >= min_size.max(2))
        .collect();
    for c in &mut clusters {
        c.sort_unstable();
    }
    clusters.sort_by_key(|c| c[0]);
    clusters
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// §11.4 consolidation pass: find clusters of ≥
/// `[epistemic] consolidation_min_sources` Active observation-class memories
/// with pairwise embedding similarity ≥ `consolidation_similarity` and no
/// pairwise NLI contradiction. Suggestion-first: clusters are returned;
/// `apply` (the `[epistemic] auto_consolidate` path) additionally creates
/// the derived Fact and demotes the sources.
///
/// Model-dependent steps run only where providers already run — with no
/// embedding or NLI provider the pass skips gracefully with a logged notice.
pub async fn consolidation_pass(
    store: &MemoryStore,
    engine: &crate::retrieval::engine::RetrievalEngine,
    config: &crate::types::EngramConfig,
    apply: bool,
) -> Result<ConsolidationReport> {
    use crate::types::{Epistemic, Status};

    let mut report = ConsolidationReport::default();
    if !engine.embeddings_available() || !engine.nli_available() {
        tracing::info!(
            "consolidation: skipped — embedding/NLI providers unavailable (graceful skip)"
        );
        report.skipped_no_providers = true;
        return Ok(report);
    }

    let min_sources = config.epistemic.consolidation_min_sources;
    let similarity = config.epistemic.consolidation_similarity;
    let ids = store.list_ids().await?;
    let loaded = store.get_batch(&ids).await?;
    let now = chrono::Utc::now();

    // Idempotence: observations already consumed by a live derived fact must
    // not re-cluster — without this, every throttled maintenance pass would
    // mint a duplicate fact from the same (demoted-but-Active) sources. If
    // the derived fact is later invalidated, its sources become eligible
    // again, which is the desired "re-derive after retraction" behavior.
    let already_derived: std::collections::HashSet<&str> = loaded
        .iter()
        .filter(|(_, m)| !m.is_invalidated_at(now))
        .filter_map(|(_, m)| m.valid_while.as_ref())
        .flat_map(|v| v.derived_from.iter().map(String::as_str))
        .collect();

    let observations: Vec<&(String, crate::types::Memory)> = loaded
        .iter()
        .filter(|(id, m)| {
            m.epistemic == Epistemic::Observation
                && m.status == Status::Active
                && !m.is_invalidated_at(now)
                && !already_derived.contains(id.as_str())
        })
        .collect();
    if observations.len() < min_sources.max(2) {
        return Ok(report);
    }
    // Pairwise-similarity bound: n observations cost n(n-1)/2 cosines. Past
    // this size the throttled maintenance pass is the wrong tool — defer with
    // a notice instead of stalling (same gated-O(n²) discipline as #58).
    const MAX_OBSERVATIONS_PER_PASS: usize = 500;
    if observations.len() > MAX_OBSERVATIONS_PER_PASS {
        tracing::info!(
            count = observations.len(),
            "consolidation: more than {MAX_OBSERVATIONS_PER_PASS} active observations; \
             skipping this pass (use compress for bulk cleanup)"
        );
        report.skipped_too_many = true;
        return Ok(report);
    }

    // Embed each observation (summary + content). Failures drop the entry.
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(observations.len());
    for (_, m) in &observations {
        let text = format!("{} {}", m.summary, m.content);
        vectors.push(engine.embed_text(&text).await);
    }

    // Pairwise similarity → union-find clusters.
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for i in 0..observations.len() {
        for j in (i + 1)..observations.len() {
            if let (Some(a), Some(b)) = (&vectors[i], &vectors[j]) {
                if cosine(a, b) >= similarity {
                    pairs.push((i, j));
                }
            }
        }
    }
    let clusters = cluster_pairs(observations.len(), &pairs, min_sources);

    // Pairwise-NLI bound per cluster: k sources cost k(k-1)/2 cross-encoder
    // inferences, so an unbounded near-duplicate cluster would stall the
    // (synchronous) maintenance pass for minutes. Oversized clusters are
    // deferred with a notice rather than half-checked — mirroring the
    // gated-O(n²) discipline from the workspace robustness pass (#58).
    const MAX_CLUSTER_SOURCES: usize = 12;

    for cluster in clusters {
        if cluster.len() > MAX_CLUSTER_SOURCES {
            tracing::info!(
                size = cluster.len(),
                "consolidation: cluster exceeds {MAX_CLUSTER_SOURCES} sources; skipping this pass \
                 (compress it manually or raise consolidation_similarity)"
            );
            continue;
        }
        // NLI gate: any pairwise contradiction disqualifies the cluster
        // (contradictory observations are a dispute, not a consolidation).
        let mut nli_pairs: Vec<(&str, &str)> = Vec::new();
        for (pos, &i) in cluster.iter().enumerate() {
            for &j in &cluster[pos + 1..] {
                nli_pairs.push((
                    observations[i].1.summary.as_str(),
                    observations[j].1.summary.as_str(),
                ));
            }
        }
        let contradicted = match engine.nli_contradictions(&nli_pairs).await {
            Some(scores) => scores
                .iter()
                .any(|s| *s as f64 >= config.nli.contradiction_threshold),
            // NLI failed mid-pass: be conservative, skip the cluster.
            None => true,
        };
        if contradicted {
            continue;
        }

        let source_ids: Vec<String> = cluster.iter().map(|&i| observations[i].0.clone()).collect();
        let summaries: Vec<String> = cluster
            .iter()
            .map(|&i| observations[i].1.summary.clone())
            .collect();

        if apply {
            match consolidate_cluster_apply(store, &source_ids, Some(engine)).await {
                Ok(new_id) => report.created.push(new_id),
                Err(e) => {
                    tracing::warn!("consolidation apply failed for {source_ids:?}: {e}");
                    continue;
                }
            }
        }
        report.clusters.push(ConsolidationCluster {
            source_ids,
            summaries,
        });
    }
    Ok(report)
}

/// Apply one consolidation cluster (§11.4): create a Fact-class memory (type
/// `context` unless all sources share a type) with
/// `valid_while.derived_from = sources`, `provenance: inferred`,
/// criticality = max(sources), decay = none — then DEMOTE the sources
/// (decay → exponential 30d, floor 0.1). Sources are never deleted: they are
/// the evidence the §10.3 derived-from check depends on.
pub async fn consolidate_cluster_apply(
    store: &MemoryStore,
    source_ids: &[String],
    engine: Option<&crate::retrieval::engine::RetrievalEngine>,
) -> Result<String> {
    use crate::types::{Epistemic, Memory, MemoryType, Provenance, Validity};

    if source_ids.len() < 2 {
        bail!("a consolidation cluster needs at least 2 sources");
    }
    let mut sources = Vec::with_capacity(source_ids.len());
    for id in source_ids {
        sources.push(store.get(id).await?);
    }

    let all_same_type = sources.windows(2).all(|w| w[0].type_ == w[1].type_);
    let common_type = if all_same_type {
        sources[0].type_
    } else {
        MemoryType::Context
    };
    let criticality = sources.iter().map(|m| m.criticality).fold(0.0f64, |a, b| {
        if b.is_finite() {
            a.max(b)
        } else {
            a
        }
    });

    let summary_max_chars = engine
        .map(crate::retrieval::engine::RetrievalEngine::summary_max_chars)
        .unwrap_or(crate::types::DEFAULT_SUMMARY_MAX_CHARS);
    let mut summary = format!("Consolidated: {}", sources[0].summary);
    if summary.chars().count() > summary_max_chars {
        // Reserve 3 chars for the ellipsis so the result still fits the bound.
        let keep = summary_max_chars.saturating_sub(3);
        summary = summary.chars().take(keep).collect::<String>() + "...";
    }
    let content = sources
        .iter()
        .map(|m| format!("- {}", m.summary))
        .collect::<Vec<_>>()
        .join("\n");

    let mut fact = Memory::new(common_type, &summary, &content, Provenance::inferred());
    fact.epistemic = Epistemic::Fact;
    fact.criticality = criticality;
    fact.decay = Some(crate::types::Decay::none());
    fact.valid_while = Some(Validity {
        derived_from: source_ids.to_vec(),
        ..Default::default()
    });
    // Union the sources' scopes so the fact applies where its evidence did.
    let mut physical: Vec<String> = sources.iter().flat_map(|m| m.physical.clone()).collect();
    physical.sort();
    physical.dedup();
    fact.physical = physical;
    let mut logical: Vec<String> = sources.iter().flat_map(|m| m.logical.clone()).collect();
    logical.sort();
    logical.dedup();
    fact.logical = logical;

    let new_id = store.create(&fact).await?;

    // Embed the derived fact so it participates in vector search immediately
    // (plain `store.create` writes no vector). Best-effort: a failed embed
    // leaves the fact index-searchable until the next reindex.
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            if let Ok(saved) = store.get(&new_id).await {
                if let Err(e) = engine.embed_memory(&saved).await {
                    tracing::warn!(memory_id = %new_id, "consolidated fact embed failed: {e}");
                }
            }
        }
    }

    // Demote sources: 30d exponential, floor 0.1 — evidence fades, never
    // vanishes.
    for id in source_ids {
        let demoted = store
            .update_with(id, |m| {
                m.decay = Some(
                    crate::types::Decay::exponential(chrono::Duration::days(30)).with_floor(0.1),
                );
                Ok(())
            })
            .await;
        if let Err(e) = demoted {
            tracing::warn!(memory_id = %id, "consolidation source demotion failed: {e}");
        }
    }
    Ok(new_id)
}

#[cfg(test)]
mod consolidation_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{DecayStrategy, Epistemic, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    #[test]
    fn cluster_pairs_union_find() {
        // 0-1-2 chained, 3-4 pair, 5 isolated.
        let pairs = [(0, 1), (1, 2), (3, 4)];
        let clusters = cluster_pairs(6, &pairs, 3);
        assert_eq!(clusters, vec![vec![0, 1, 2]]);
        let clusters = cluster_pairs(6, &pairs, 2);
        assert_eq!(clusters, vec![vec![0, 1, 2], vec![3, 4]]);
        // Out-of-range pairs are ignored; empty input yields nothing.
        assert!(cluster_pairs(2, &[(0, 5)], 2).is_empty());
        assert!(cluster_pairs(0, &[], 2).is_empty());
    }

    #[tokio::test]
    async fn consolidation_skips_without_providers() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let engine = crate::retrieval::engine::RetrievalEngine::new(
            store.clone(),
            crate::types::EngramConfig::default(),
        );
        let config = crate::types::EngramConfig::default();
        let report = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert!(report.skipped_no_providers);
        assert!(report.clusters.is_empty());
    }

    #[tokio::test]
    async fn consolidate_cluster_apply_creates_fact_and_demotes_sources() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        for (id, crit) in [("con-a", 0.4), ("con-b", 0.7), ("con-c", 0.5)] {
            let mut m = Memory::new(
                MemoryType::Debug,
                format!("Observation {id}"),
                "body",
                Provenance::human(),
            );
            m.id = id.to_string();
            m.criticality = crit;
            m.physical = vec![format!("src/{id}.rs")];
            store.create(&m).await.unwrap();
        }

        let ids: Vec<String> = ["con-a", "con-b", "con-c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let new_id = consolidate_cluster_apply(&store, &ids, None).await.unwrap();

        let fact = store.get(&new_id).await.unwrap();
        assert_eq!(fact.epistemic, Epistemic::Fact);
        assert_eq!(fact.type_, MemoryType::Debug, "all sources share a type");
        assert_eq!(fact.criticality, 0.7, "max of sources");
        assert_eq!(
            fact.valid_while.as_ref().unwrap().derived_from,
            ids,
            "derivation links recorded for the §10.3 cascade"
        );
        assert_eq!(
            fact.provenance.source,
            crate::types::ProvenanceSource::Inferred
        );
        assert_eq!(fact.decay.as_ref().unwrap().strategy, DecayStrategy::None);
        assert_eq!(fact.physical.len(), 3, "scope union");

        // Sources demoted, never deleted.
        for id in &ids {
            let m = store.get(id).await.unwrap();
            let decay = m.decay.unwrap();
            assert_eq!(decay.strategy, DecayStrategy::Exponential);
            assert_eq!(decay.half_life, Some(chrono::Duration::days(30)));
            assert_eq!(decay.floor, 0.1);
        }

        // Mixed types fall back to Context.
        let mut other = Memory::new(MemoryType::Convention, "Other", "b", Provenance::human());
        other.id = "con-d".to_string();
        store.create(&other).await.unwrap();
        let mixed: Vec<String> = vec!["con-a".into(), "con-d".into()];
        let mixed_id = consolidate_cluster_apply(&store, &mixed, None)
            .await
            .unwrap();
        assert_eq!(
            store.get(&mixed_id).await.unwrap().type_,
            MemoryType::Context
        );
    }

    // --- Gate tests: stub providers so similarity + NLI gating is
    // --- deterministic without loading any real model.

    /// Deterministic embeddings: texts containing the same `group<X>` marker
    /// share an identical (cosine 1.0) vector; different markers are
    /// orthogonal (cosine 0.0).
    struct MarkerEmbedding;

    #[async_trait::async_trait]
    impl crate::embeddings::EmbeddingProvider for MarkerEmbedding {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 384];
            if text.contains("groupA") {
                v[0] = 1.0;
            } else if text.contains("groupB") {
                v[1] = 1.0;
            } else {
                v[2] = 1.0;
            }
            Ok(v)
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }
        fn dimensions(&self) -> usize {
            384
        }
        fn max_tokens(&self) -> usize {
            256
        }
        fn model_id(&self) -> String {
            "onnx/marker-stub".to_string()
        }
    }

    /// Stub NLI: any pair where either side contains "flaky" is a full
    /// contradiction; everything else is neutral.
    struct MarkerNli;

    #[async_trait::async_trait]
    impl crate::nli::NliProvider for MarkerNli {
        async fn classify(
            &self,
            premise: &str,
            hypothesis: &str,
        ) -> anyhow::Result<crate::nli::NliResult> {
            let contradicted = premise.contains("flaky") || hypothesis.contains("flaky");
            Ok(crate::nli::NliResult {
                label: if contradicted {
                    crate::nli::NliLabel::Contradiction
                } else {
                    crate::nli::NliLabel::Neutral
                },
                entailment: 0.0,
                neutral: if contradicted { 0.0 } else { 1.0 },
                contradiction: if contradicted { 1.0 } else { 0.0 },
            })
        }
        async fn classify_batch(
            &self,
            pairs: &[(&str, &str)],
        ) -> anyhow::Result<Vec<crate::nli::NliResult>> {
            let mut out = Vec::with_capacity(pairs.len());
            for (p, h) in pairs {
                out.push(self.classify(p, h).await?);
            }
            Ok(out)
        }
    }

    async fn gate_fixture() -> (
        TempDir,
        MemoryStore,
        crate::retrieval::engine::RetrievalEngine,
        crate::types::EngramConfig,
    ) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mut config = crate::types::EngramConfig::default();
        config.nli.enabled = true;
        let engine = crate::retrieval::engine::RetrievalEngine::new(store.clone(), config.clone())
            .with_embedding_provider(std::sync::Arc::new(MarkerEmbedding))
            .with_nli_provider(std::sync::Arc::new(MarkerNli));
        (tmp, store, engine, config)
    }

    async fn observation(store: &MemoryStore, id: &str, summary: &str) {
        // Debug is diagonally Observation-class.
        let mut m = Memory::new(MemoryType::Debug, summary, summary, Provenance::human());
        m.id = id.to_string();
        store.create(&m).await.unwrap();
    }

    /// The §11.4 similarity gate: only observations whose embeddings clear
    /// `consolidation_similarity` cluster; sub-threshold (orthogonal)
    /// observations never do, and clusters below `consolidation_min_sources`
    /// are dropped.
    #[tokio::test]
    async fn consolidation_gate_clusters_by_similarity_only() {
        let (_t, store, engine, config) = gate_fixture().await;

        for id in ["ga-1", "ga-2", "ga-3"] {
            observation(&store, id, &format!("groupA behavior seen in {id}")).await;
        }
        // Only two of these — below min_sources (3) — plus orthogonal class.
        for id in ["gb-1", "gb-2"] {
            observation(&store, id, &format!("groupB behavior seen in {id}")).await;
        }

        let report = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert!(!report.skipped_no_providers);
        assert_eq!(report.clusters.len(), 1, "only the 3-strong groupA cluster");
        let mut ids = report.clusters[0].source_ids.clone();
        ids.sort();
        assert_eq!(ids, vec!["ga-1", "ga-2", "ga-3"]);
        assert!(report.created.is_empty(), "suggestion mode creates nothing");
    }

    /// The §11.4 NLI gate: a similarity cluster containing a pairwise
    /// contradiction is a dispute, not a consolidation — it must be dropped.
    #[tokio::test]
    async fn consolidation_gate_rejects_contradicting_cluster() {
        let (_t, store, engine, config) = gate_fixture().await;

        observation(&store, "gc-1", "groupA the cache is fast").await;
        observation(&store, "gc-2", "groupA the cache is quick").await;
        // Same embedding group, but the stub NLI contradicts this one.
        observation(&store, "gc-3", "groupA the cache is flaky").await;

        let report = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert!(
            report.clusters.is_empty(),
            "contradicting cluster must not consolidate: {:?}",
            report.clusters
        );
    }

    /// Idempotence: after an applied consolidation, the (still-Active,
    /// demoted) sources must not re-cluster on the next pass — one derived
    /// fact, not one per maintenance interval.
    #[tokio::test]
    async fn consolidation_apply_is_idempotent_across_passes() {
        let (_t, store, engine, config) = gate_fixture().await;

        for id in ["gi-1", "gi-2", "gi-3"] {
            observation(&store, id, &format!("groupA metric drift in {id}")).await;
        }

        let first = consolidation_pass(&store, &engine, &config, true)
            .await
            .unwrap();
        assert_eq!(first.created.len(), 1, "first pass consolidates");
        let fact = store.get(&first.created[0]).await.unwrap();
        assert_eq!(fact.epistemic, Epistemic::Fact);

        let second = consolidation_pass(&store, &engine, &config, true)
            .await
            .unwrap();
        assert!(
            second.created.is_empty() && second.clusters.is_empty(),
            "consumed sources must not re-cluster: {:?}",
            second.clusters
        );

        // Invalidating the derived fact frees its sources to re-derive.
        store
            .invalidate_with(&first.created[0], None, chrono::Utc::now())
            .await
            .unwrap();
        let third = consolidation_pass(&store, &engine, &config, false)
            .await
            .unwrap();
        assert_eq!(
            third.clusters.len(),
            1,
            "retracted derivation reopens the cluster"
        );
    }

    /// O(n²) bound (#58 discipline): a cluster larger than the per-pass NLI
    /// budget is deferred with a notice, not half-checked or consolidated.
    #[tokio::test]
    async fn consolidation_defers_oversized_clusters() {
        let (_t, store, engine, config) = gate_fixture().await;

        // 13 same-group observations: one cluster of 13 > MAX_CLUSTER_SOURCES.
        for i in 0..13 {
            observation(
                &store,
                &format!("gx-{i}"),
                &format!("groupA repeated pattern {i}"),
            )
            .await;
        }

        let report = consolidation_pass(&store, &engine, &config, true)
            .await
            .unwrap();
        assert!(
            report.clusters.is_empty() && report.created.is_empty(),
            "oversized cluster must be deferred: {:?}",
            report.clusters
        );
    }
}
