//! Unified memory query command.

use crate::engine::engine_for;
use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::daemon::{DaemonCell, DaemonPolicy};
use engramdb::ops::validate_score;
use engramdb::retrieval::engine::{RetrievalMode, RetrievalQuery, RetrievalResult};
use engramdb::storage::MemoryStore;
use std::path::Path;
use std::sync::Arc;

/// Parameters for the query command.
pub struct QueryParams {
    pub mode: RetrievalMode,
    pub query: Option<String>,
    pub path: Option<String>,
    pub logical: Vec<String>,
    pub type_filter: Vec<String>,
    pub tags: Vec<String>,
    pub min_criticality: Option<f64>,
    pub max_results: usize,
    pub detail_level: Option<String>,
    pub include_expired: bool,
    pub epistemic: Vec<String>,
    pub situation: Option<String>,
    pub include_invalidated: bool,
    pub show_scores: bool,
    /// Also merge global-store memories into the project results.
    pub include_global: bool,
}

/// Run a unified retrieval query.
///
/// `mode: Rank` behaves like the old `retrieve` flow — returns every
/// memory passing the type/tag/criticality/physical filters, scored and
/// sorted. `mode: Filter` requires at least one positive relevance signal
/// (keyword, semantic, scope proximity, or tag match).
pub async fn run_query(
    dir: &Path,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }

    let show_scores = params.show_scores;
    // Capture the bits needed for an empty-result hint before `params` is moved.
    let mode = params.mode;
    let had_query = params.query.as_ref().is_some_and(|q| !q.is_empty());
    let result =
        compute_query_result(store, global, params, embedding_backend, cell, policy).await?;

    formatter.print_retrieval_result(&result, show_scores);

    // Explain an empty result rather than leaving the user at `total: 0`.
    // A free-text query in `rank` mode is scored (semantic + keyword) but
    // gated by `retrieval.relevance_threshold` (default 0.45); memories that
    // match only weakly fall below it and silently vanish. `filter` mode uses
    // a looser threshold and surfaces keyword/tag/scope matches. The hint is a
    // no-op in JSON mode (structured output speaks for itself).
    if result.memories.is_empty() && had_query {
        match mode {
            RetrievalMode::Rank => formatter.print_hint(
                "No memories cleared the rank relevance threshold. Try `--mode filter` for \
                 free-text search, broaden the query, or lower `retrieval.relevance_threshold` \
                 in .engramdb/config.toml.",
            ),
            RetrievalMode::Filter => formatter.print_hint(
                "No memories matched. Try broader terms, add `--path`/`--logical` context, or \
                 `--mode rank` to browse everything by score.",
            ),
        }
    }

    Ok(())
}

/// Build the retrieval engine for `store`, run `params` against it, and
/// (when `params.include_global` is set and we are not already querying the
/// global store) merge in global-store hits — mirroring the MCP
/// `include_global` option. Returned separately from rendering so the merge
/// behavior is unit-testable offline.
async fn compute_query_result(
    store: MemoryStore,
    global: bool,
    params: QueryParams,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<RetrievalResult> {
    let engine = engine_for(store, embedding_backend, cell, policy).await;

    let types = engramdb::ops::parse_type_filter(Some(&params.type_filter))?;

    let tags = if !params.tags.is_empty() {
        Some(params.tags)
    } else {
        None
    };

    let detail_level =
        engramdb::ops::parse_detail_level_or_default(params.detail_level.as_deref())?;

    if let Some(mc) = params.min_criticality {
        validate_score(mc, "min_criticality")?;
    }

    let query = RetrievalQuery {
        mode: params.mode,
        path: params.path,
        logical: params.logical,
        query: params.query,
        types,
        tags,
        min_criticality: params.min_criticality,
        max_results: Some(params.max_results),
        include_expired: Some(params.include_expired),
        detail_level,
        epistemic: engramdb::ops::parse_epistemic_filter(Some(&params.epistemic))?,
        situation: params
            .situation
            .as_deref()
            .map(engramdb::ops::parse_situation)
            .transpose()?,
        include_invalidated: Some(params.include_invalidated),
    };

    // Optionally fold in global-store memories via the shared band
    // (ops::query_memories_with_global). Skipped when already querying the
    // global store (`--global`) — nothing extra to merge.
    let include_global = params.include_global && !global;
    let result =
        engramdb::ops::query_memories_with_global(&engine, &query, include_global, || async {
            let global_store = MemoryStore::open_global().await.ok()?;
            Some(engine_for(global_store, embedding_backend, cell, policy).await)
        })
        .await?;

    Ok(result)
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

    fn base_params() -> QueryParams {
        QueryParams {
            epistemic: vec![],
            situation: None,
            include_invalidated: false,
            mode: RetrievalMode::Rank,
            query: None,
            path: None,
            logical: vec![],
            type_filter: vec![],
            tags: vec![],
            min_criticality: None,
            max_results: 10,
            detail_level: None,
            include_expired: false,
            show_scores: false,
            include_global: false,
        }
    }

    #[tokio::test]
    async fn test_query_rank_returns_ok() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            path: Some("src/main.rs".to_string()),
            ..base_params()
        };

        let result = run_query(
            temp_dir.path(),
            false,
            params,
            None,
            &formatter,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_query_rank_invalid_detail_level() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            path: Some("src/main.rs".to_string()),
            detail_level: Some("invalid".to_string()),
            ..base_params()
        };

        let result = run_query(
            temp_dir.path(),
            false,
            params,
            None,
            &formatter,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid detail level"));
    }

    #[tokio::test]
    async fn test_query_rank_with_type_filter() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            path: Some("src/main.rs".to_string()),
            type_filter: vec!["decision".to_string()],
            ..base_params()
        };

        let result = run_query(
            temp_dir.path(),
            false,
            params,
            None,
            &formatter,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_query_filter_with_text() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        let params = QueryParams {
            mode: RetrievalMode::Filter,
            query: Some("Test".to_string()),
            ..base_params()
        };

        let result = run_query(
            temp_dir.path(),
            false,
            params,
            None,
            &formatter,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_include_global_merges_global_hits_with_negative_control() {
        // Serialize global-store access and start from a clean global store.
        let _lock = engramdb::storage::test_support::acquire_global_test_lock().await;
        let global = MemoryStore::open_global().await.unwrap();
        global
            .create(&create_test_memory(
                "global-q-001-zzz",
                MemoryType::Decision,
                0.95,
            ))
            .await
            .unwrap();

        let (temp_dir, _store) = setup_test_store().await;

        // Rank mode with no query text needs no embeddings → fully offline.
        let mk = |include_global: bool| QueryParams {
            mode: RetrievalMode::Rank,
            path: Some("src/main.rs".to_string()),
            include_global,
            ..base_params()
        };

        // include_global = true → the global memory is folded in.
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let merged = compute_query_result(
            store,
            false,
            mk(true),
            None,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await
        .unwrap();
        assert!(
            merged
                .memories
                .iter()
                .any(|m| m.memory.id == "global-q-001-zzz"),
            "global memory should be merged when include_global=true"
        );

        // include_global = false → negative control: it must NOT appear.
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let project_only = compute_query_result(
            store,
            false,
            mk(false),
            None,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await
        .unwrap();
        assert!(
            !project_only
                .memories
                .iter()
                .any(|m| m.memory.id == "global-q-001-zzz"),
            "global memory must NOT appear when include_global=false"
        );
    }

    #[tokio::test]
    async fn test_query_filter_requires_signal() {
        let (temp_dir, _store) = setup_test_store().await;
        let formatter = OutputFormatter::new(None, false, true);

        // No query, logical, path, or tags — must error.
        let params = QueryParams {
            mode: RetrievalMode::Filter,
            min_criticality: Some(0.5),
            ..base_params()
        };

        let result = run_query(
            temp_dir.path(),
            false,
            params,
            None,
            &formatter,
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("filter requires at least one"),
            "expected validation message, got: {}",
            msg
        );
    }
}
