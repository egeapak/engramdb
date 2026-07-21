//! Memory creation operation.

use crate::ops::parse_decay_strategy;
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use crate::title::TitleStrategy;
use crate::types::{
    Decay, DecayStrategy, Epistemic, Generality, Memory, MemoryType, Provenance, Validity,
    Visibility,
};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

/// Parameters for creating a new memory.
pub struct CreateParams {
    pub type_: MemoryType,
    pub content: String,
    pub summary: String,
    pub title: Option<String>,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: f64,
    pub confidence: f64,
    pub details: Option<String>,
    pub visibility: Visibility,
    pub provenance: Provenance,
    pub supersedes: Vec<String>,
    /// Per-memory sharing precision for a memory written into a group or the
    /// everyone/global store: the project ids and/or group ids that may see it.
    /// `None` ⇒ visible to the whole store's group (the common case). `Some`
    /// restricts it to the listed audience (see [`crate::ops::audience_allows`]).
    /// Inert on a project-local memory (audience is only consulted when a shared
    /// store is fanned in), so it is meaningful only alongside a group/global
    /// write.
    pub audience: Option<Vec<String>>,
    /// Epistemic class; `None` ⇒ `type_.default_epistemic()`.
    pub epistemic: Option<Epistemic>,
    /// Free-text premise the memory depends on (`valid_while.premise`).
    pub premise: Option<String>,
    /// Paths/globs whose change invalidates the memory
    /// (`valid_while.invalidated_by` — distinct from `physical`).
    pub invalidated_by: Vec<String>,
    /// Task/feature the memory was created for (`valid_while.origin_task`).
    pub origin_task: Option<String>,
    /// How far beyond its origin the memory holds; `None` ⇒ `Project`.
    pub generality: Option<Generality>,
    /// Valid-time start (§2.4 backdating; rare). `None` ⇒ `created_at`.
    pub valid_from: Option<DateTime<Utc>>,
    pub decay_strategy: Option<String>,
    pub decay_half_life: Option<u64>,
    pub decay_ttl: Option<u64>,
    pub decay_floor: Option<f64>,
    pub title_strategy: TitleStrategy,
    /// When `true`, run embedding + contradiction detection in a background
    /// `tokio::spawn`ed task and return immediately. Errors in the background
    /// task are logged via `tracing` and never surface to the caller. Used by
    /// the MCP `create` tool so the agent isn't blocked on embedding inference.
    ///
    /// When `false` (default), embedding and contradiction detection run inline
    /// before the function returns — preserving the synchronous ergonomics that
    /// CLI commands and tests rely on.
    pub embed_async: bool,
}

/// Result of a create operation.
#[derive(Debug)]
pub struct CreateResult {
    pub id: String,
    pub summary: String,
}

/// Resolve a title for `text` under `strategy`, preferring a cached/pooled
/// T5 generator carried by `engine` (loaded once into the provider bundle —
/// or, with the daemon, served by it) over building a fresh
/// encoder+decoder ONNX session on this `create`. Keyword/`none` stay on the
/// lightweight in-process path. Error/empty handling mirrors
/// [`crate::title::generate_title`] so behavior is identical bar the speed.
async fn title_for(
    engine: Option<&RetrievalEngine>,
    strategy: TitleStrategy,
    text: &str,
) -> Option<String> {
    if strategy == TitleStrategy::T5 {
        if let Some(generator) = engine.and_then(|e| e.title_generator()) {
            return match generator.generate(text).await {
                Ok(title) if !title.is_empty() => Some(title),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!("cached T5 title generation failed: {}", e);
                    None
                }
            };
        }
    }
    crate::title::generate_title(strategy, text).await
}

/// Validate that a summary is non-empty, single-line, and within
/// `max_chars` characters. Single-line matters beyond cosmetics: the
/// memory-file writer renders the summary on one structural line (H1 or
/// `**Summary:**`), so embedded newlines would be collapsed on write anyway —
/// reject them up front instead of silently rewriting the caller's text.
///
/// `max_chars` comes from `[content].summary_max_chars`
/// (default [`crate::types::DEFAULT_SUMMARY_MAX_CHARS`]); callers without a
/// resolved config should pass that default. The bound is measured in
/// characters, not bytes, so multibyte summaries are not penalized.
pub fn validate_summary(summary: &str, max_chars: usize) -> Result<()> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Summary cannot be empty");
    }
    if trimmed.contains(['\n', '\r']) {
        anyhow::bail!("Summary must be a single line");
    }
    let len = trimmed.chars().count();
    if len > max_chars {
        anyhow::bail!("Summary must be <= {max_chars} characters (got {len})");
    }
    Ok(())
}

/// Create a new memory in the store.
///
/// If `engine` is provided and has embeddings available, the memory is
/// automatically embedded into the vector store after creation.
pub async fn create_memory(
    store: &MemoryStore,
    params: CreateParams,
    engine: Option<&RetrievalEngine>,
) -> Result<CreateResult> {
    let summary_max_chars = engine
        .map(RetrievalEngine::summary_max_chars)
        .unwrap_or(crate::types::DEFAULT_SUMMARY_MAX_CHARS);
    validate_summary(&params.summary, summary_max_chars)?;
    // Validate scores here, in the shared ops core, so EVERY create path
    // (direct CLI flags, interactive prompts, editor, MCP) is covered — not
    // just the CLI direct path. Out-of-range/NaN scores corrupt the scoring
    // math that assumes the [0,1] domain (finding #3).
    super::validate_score(params.criticality, "criticality")?;
    super::validate_score(params.confidence, "confidence")?;
    // update_memory validates this too — create must not be the one path
    // that lets a NaN/out-of-range floor into the decay math (the [0,1]
    // clamp on the final score does not survive a NaN floor).
    if let Some(floor) = params.decay_floor {
        super::validate_score(floor, "decay_floor")?;
    }
    let summary = params.summary;

    // Use default physical scope if empty
    let mut physical = if params.physical.is_empty() {
        vec!["/".to_string()]
    } else {
        params.physical
    };

    // Scope hygiene (cross-repo correctness): physical scopes are repo-relative
    // file paths, meaningless in any other repo. A group/everyone(global) store
    // is shared across repos, so a stored physical path would earn (or lose)
    // physical-proximity score against a foreign repo's layout. Strip it on the
    // write path so the record carries no misleading physical scope; logical
    // scope (repo-independent dot-notation) is retained. This mirrors the
    // read-side suppression in `query_memories_with_extra_stores`. See the
    // multi-project-memories design doc's scope-hygiene note.
    if store.is_group() || store.is_global() {
        physical.clear();
    }

    // Build memory
    let mut memory = Memory::new(params.type_, &summary, &params.content, params.provenance);

    // Resolve the epistemic class (type-derived default) and the
    // two-dimensional decay default (§2.6): the declared class wins over the
    // type default when off-diagonal. `Memory::new` already set the diagonal
    // `type_.default_decay()`; only replace it when the class differs. The
    // effective off-diagonal Observation curve comes from `[epistemic]`
    // config when an engine is available (§2.6 config note), falling back to
    // the built-in constants (90d half-life, floor 0.2) — explicit
    // user-provided decay below still wins over both.
    let epistemic = params
        .epistemic
        .unwrap_or_else(|| params.type_.default_epistemic());
    memory.epistemic = epistemic;
    if epistemic != params.type_.default_epistemic() {
        memory.decay = match epistemic {
            Epistemic::Observation => {
                let (half_life_days, floor) = engine
                    .map(|e| {
                        let cfg = &e.config().epistemic;
                        (cfg.observation_half_life_days, cfg.observation_decay_floor)
                    })
                    .unwrap_or((90, 0.2));
                Some(Decay::exponential(Duration::days(half_life_days as i64)).with_floor(floor))
            }
            _ => crate::types::default_decay(params.type_, epistemic),
        };
    }

    // Assemble the validity condition; an all-empty Validity stays None.
    let valid_while = Validity {
        premise: params.premise,
        invalidated_by: params.invalidated_by,
        origin_task: params.origin_task,
        generality: params.generality.unwrap_or_default(),
        derived_from: vec![],
    };
    memory.valid_while = (!valid_while.is_empty()).then_some(valid_while);
    memory.valid_from = params.valid_from;
    // Auto-generate title if not provided and strategy is not None
    let title = if params.title.is_some() {
        params.title
    } else {
        // Use summary as input for title generation (it's concise and descriptive).
        title_for(engine, params.title_strategy, &summary).await
    };
    memory.title = title;
    memory.physical = physical;
    memory.logical = params.logical;
    memory.tags = params.tags;
    memory.criticality = params.criticality;
    memory.confidence = params.confidence;
    memory.details = params.details;
    memory.visibility = params.visibility;
    memory.supersedes = params.supersedes;
    // Normalize an empty audience list to `None` (whole-group visibility) so an
    // empty `--audience` never accidentally hides a memory from everyone.
    memory.audience = params.audience.filter(|a| !a.is_empty());

    // Apply custom decay config if any decay fields are provided
    if params.decay_strategy.is_some()
        || params.decay_half_life.is_some()
        || params.decay_ttl.is_some()
        || params.decay_floor.is_some()
    {
        let strategy = if let Some(ref strategy_str) = params.decay_strategy {
            parse_decay_strategy(strategy_str)?
        } else {
            // Keep default strategy from memory type if not specified
            memory
                .decay
                .as_ref()
                .map(|d| d.strategy.clone())
                .unwrap_or(DecayStrategy::None)
        };

        let mut decay = Decay::new(strategy);

        // Apply numeric fields
        if let Some(half_life_secs) = params.decay_half_life {
            decay.half_life = Some(Duration::seconds(half_life_secs as i64));
        } else if let Some(existing_decay) = &memory.decay {
            decay.half_life = existing_decay.half_life;
        }

        if let Some(ttl_secs) = params.decay_ttl {
            decay.ttl = Some(Duration::seconds(ttl_secs as i64));
        } else if let Some(existing_decay) = &memory.decay {
            decay.ttl = existing_decay.ttl;
        }

        if let Some(floor) = params.decay_floor {
            decay.floor = floor;
        } else if let Some(existing_decay) = &memory.decay {
            decay.floor = existing_decay.floor;
        }

        memory.decay = Some(decay);
    }

    // Compute expires_at from decay TTL if applicable
    if let Some(ref decay) = memory.decay {
        if let Some(ttl) = decay.ttl {
            memory.expires_at = Some(memory.created_at + ttl);
        }
    }

    let id = store.create(&memory).await?;

    // Supersession closes validity windows (§2.4 writer 1): each referenced
    // live memory gets `invalidated_at = now`, `superseded_by = <new id>`.
    // Runs after the new memory persists so a failed create never closes
    // anything.
    super::close_superseded_windows(store, &memory.supersedes, &id).await;

    // Run embedding + contradiction detection. The metadata index is already
    // updated synchronously inside `store.create` (filter queries see the new
    // memory immediately); only vector embedding and NLI classification are
    // gated on `embed_async`.
    //
    // - `embed_async = true`  (MCP path): spawn a fire-and-forget task so the
    //                                     agent isn't blocked on embedding-model
    //                                     inference. Vector search may briefly
    //                                     not include the new memory until the
    //                                     task finishes.
    // - `embed_async = false` (CLI / tests / compress): run inline so callers
    //                                                   that exit or assert
    //                                                   immediately after
    //                                                   create see a consistent
    //                                                   state.
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            if params.embed_async {
                // memory.id == id (set by Memory::new), so we can move the
                // local memory into the spawned task without re-reading from
                // disk. The local `summary` variable carries the value through
                // to the returned CreateResult below.
                let _ = engine.spawn_ingest(memory);
            } else {
                // `memory.id == id` (set by Memory::new), so embed the
                // in-hand value directly — the old `store.get(&id)` re-read
                // was a redundant dir-scan+parse of a file we just wrote.
                //
                // Embed failure is non-fatal, matching the async branch: the
                // memory is already durably created (file + index row), so
                // returning Err here would tell the caller the create failed
                // and the natural retry would write a DUPLICATE memory. The
                // memory degrades to keyword-only retrieval until a reindex.
                let saved = memory;
                if let Err(e) = engine.embed_memory(&saved).await {
                    tracing::warn!(
                        memory_id = %saved.id,
                        "memory created but embedding failed (semantic search \
                         will miss it until `engramdb reindex --embeddings-only`): {e}"
                    );
                }

                // Detect contradictions with existing memories (best-effort).
                // Challenge writes are spawned so create_memory returns without
                // waiting for them; per-write errors are logged inside the
                // helper.
                if engine.nli_available() {
                    if let Ok(contradictions) = engine.detect_contradictions(&saved).await {
                        if !contradictions.is_empty() {
                            tracing::debug!(
                                memory_id = %saved.id,
                                count = contradictions.len(),
                                "NLI detected contradictions with existing memories"
                            );
                            let store_clone = store.clone();
                            let new_meta = crate::ops::NewMemoryMeta::from(&saved);
                            tokio::spawn(async move {
                                crate::ops::challenge_for_contradictions(
                                    &store_clone,
                                    &new_meta,
                                    &contradictions,
                                )
                                .await;
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(CreateResult { id, summary })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{DecayStrategy, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    async fn setup_test_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        (temp_dir, store)
    }

    fn minimal_create_params() -> CreateParams {
        CreateParams {
            type_: MemoryType::Decision,
            content: "Test content".to_string(),
            summary: "Test summary".to_string(),
            title: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Shared,
            provenance: Provenance::human(),
            supersedes: vec![],
            audience: None,
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
        }
    }

    // Finding #3: create_memory validates criticality/confidence in the shared
    // ops core, so every front-end path (interactive/editor/MCP — not just the
    // CLI direct path) rejects out-of-range/NaN scores.
    #[tokio::test]
    async fn create_memory_rejects_out_of_range_scores() {
        let (_t, store) = setup_test_store().await;

        // POSITIVE: valid scores succeed.
        assert!(create_memory(&store, minimal_create_params(), None)
            .await
            .is_ok());

        // NEGATIVE (red before fix): criticality > 1.0 is rejected.
        let mut p = minimal_create_params();
        p.criticality = 5.0;
        assert!(create_memory(&store, p, None).await.is_err());

        // NEGATIVE: confidence < 0.0 is rejected.
        let mut p = minimal_create_params();
        p.confidence = -0.5;
        assert!(create_memory(&store, p, None).await.is_err());

        // NEGATIVE: NaN criticality is rejected.
        let mut p = minimal_create_params();
        p.criticality = f64::NAN;
        assert!(create_memory(&store, p, None).await.is_err());
    }

    // Scope hygiene: a memory created into a group (or everyone/global) store
    // must not carry a physical scope, even when physical paths were supplied —
    // repo-relative paths are meaningless cross-repo. See the strip in
    // `create_memory` and the multi-project-memories design doc.
    #[tokio::test]
    async fn create_into_group_store_strips_physical_scope() {
        let group_id = crate::storage::paths::compute_group_id("scope-hygiene-test");
        let store = MemoryStore::init_group(&group_id).await.unwrap();

        let mut params = minimal_create_params();
        params.physical = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
        params.logical = vec!["backend.api".to_string()];

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        // Physical scope stripped despite being supplied.
        assert!(
            memory.physical.is_empty(),
            "group-store memory must have empty physical scope, got {:?}",
            memory.physical
        );
        // Logical scope (repo-independent) is retained.
        assert_eq!(memory.logical, vec!["backend.api".to_string()]);
    }

    // The audience write path (multi-project memories): a non-empty
    // `CreateParams.audience` is persisted onto the memory so the read-side
    // `audience_allows` filter can enforce per-memory sharing precision.
    #[tokio::test]
    async fn create_persists_audience() {
        let group_id = crate::storage::paths::compute_group_id("audience-create-test");
        let store = MemoryStore::init_group(&group_id).await.unwrap();

        let mut params = minimal_create_params();
        params.audience = Some(vec!["projA".to_string(), "__g_x".to_string()]);

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();
        assert_eq!(
            memory.audience,
            Some(vec!["projA".to_string(), "__g_x".to_string()])
        );
    }

    // An empty audience list must normalize to `None` (whole-group visibility)
    // rather than persist an empty audience that would hide the memory from
    // everyone — the guard in `create_memory`.
    #[tokio::test]
    async fn create_normalizes_empty_audience_to_none() {
        let (_t, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        params.audience = Some(vec![]);

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();
        assert_eq!(memory.audience, None);
    }

    /// Title generator that returns a fixed string, to prove `title_for`
    /// routes T5 through the cached provider rather than building one.
    struct StubTitle(&'static str);
    #[async_trait::async_trait]
    impl crate::title::TitleGenerator for StubTitle {
        async fn generate(&self, _text: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[tokio::test]
    async fn title_for_prefers_cached_t5_then_falls_back_by_strategy() {
        let (_t, store) = setup_test_store().await;
        let engine = RetrievalEngine::new(store, crate::types::EngramConfig::default())
            .with_title_provider(std::sync::Arc::new(StubTitle("cached t5 title")));

        // T5 + a cached generator → use it (no fresh model build).
        assert_eq!(
            title_for(Some(&engine), TitleStrategy::T5, "anything").await,
            Some("cached t5 title".to_string())
        );

        // Keyword never touches the cached T5 generator — lightweight path.
        let kw = title_for(
            Some(&engine),
            TitleStrategy::Keyword,
            "the quick brown fox jumps over the lazy dog",
        )
        .await;
        assert!(
            kw.is_some() && kw.as_deref() != Some("cached t5 title"),
            "keyword must use RAKE, not the cached T5 stub (got {kw:?})"
        );

        // None → no automatic title.
        assert_eq!(
            title_for(Some(&engine), TitleStrategy::None, "anything").await,
            None
        );

        // T5 requested but no cached generator and no engine → falls back to
        // the ad-hoc path (returns None here only if the model is absent;
        // the point is it must not panic and must not use the stub).
        let no_engine = title_for(None, TitleStrategy::None, "anything").await;
        assert_eq!(no_engine, None);
    }

    #[tokio::test]
    async fn test_create_memory_with_custom_decay_all_fields() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("exponential".to_string());
        params.decay_half_life = Some(604800); // 7 days in seconds
        params.decay_floor = Some(0.3);

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential);
        assert_eq!(decay.half_life, Some(Duration::seconds(604800)));
        assert_eq!(decay.floor, 0.3);
    }

    #[tokio::test]
    async fn test_create_memory_with_partial_decay_only_strategy() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("linear".to_string());

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Linear);
        // Other fields should be None/0.0 since not specified
        assert_eq!(decay.floor, 0.0);
    }

    #[tokio::test]
    async fn test_create_memory_with_partial_decay_only_floor() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_floor = Some(0.5);

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        // Should keep default strategy (None for Decision type)
        assert_eq!(decay.strategy, DecayStrategy::None);
        assert_eq!(decay.floor, 0.5);
    }

    #[tokio::test]
    async fn test_create_memory_invalid_decay_strategy_returns_error() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("invalid_strategy".to_string());

        let result = create_memory(&store, params, None).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid decay strategy"));
    }

    #[tokio::test]
    async fn test_create_memory_decay_config_persists_through_save_load() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("step".to_string());
        params.decay_ttl = Some(2592000); // 30 days in seconds
        params.decay_floor = Some(0.2);

        let result = create_memory(&store, params, None).await.unwrap();

        // Reload from disk
        let memory = store.get(&result.id).await.unwrap();

        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Step);
        assert_eq!(decay.ttl, Some(Duration::seconds(2592000)));
        assert_eq!(decay.floor, 0.2);
    }

    #[tokio::test]
    async fn test_create_memory_no_decay_fields_uses_type_default() {
        let (_temp, store) = setup_test_store().await;

        let params = minimal_create_params();

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        // Decision type has default decay of None
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::None);
    }

    #[tokio::test]
    async fn test_create_memory_with_linear_decay_and_ttl() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("linear".to_string());
        params.decay_ttl = Some(86400); // 1 day in seconds

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Linear);
        assert_eq!(decay.ttl, Some(Duration::seconds(86400)));
        assert_eq!(decay.half_life, None); // Should be None for linear
    }

    #[tokio::test]
    async fn test_create_memory_sets_expires_at_from_ttl() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("linear".to_string());
        params.decay_ttl = Some(3600); // 1 hour

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        assert!(
            memory.expires_at.is_some(),
            "expires_at should be set when decay has a TTL"
        );
        let expires_at = memory.expires_at.unwrap();
        let expected = memory.created_at + Duration::seconds(3600);
        assert!(
            (expires_at - expected).num_seconds().abs() <= 1,
            "expires_at should be created_at + TTL"
        );
    }

    #[tokio::test]
    async fn test_create_memory_no_expires_at_without_ttl() {
        let (_temp, store) = setup_test_store().await;

        let mut params = minimal_create_params();
        params.decay_strategy = Some("exponential".to_string());
        params.decay_half_life = Some(604800); // 7 days, no TTL

        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();

        assert!(
            memory.expires_at.is_none(),
            "expires_at should be None when decay has no TTL"
        );
    }

    #[test]
    fn test_validate_summary_rejects_empty() {
        use crate::types::DEFAULT_SUMMARY_MAX_CHARS as MAX;
        assert!(validate_summary("", MAX).is_err());
        assert!(validate_summary("   ", MAX).is_err());
        assert!(validate_summary("\n\t", MAX).is_err());
    }

    #[test]
    fn test_validate_summary_rejects_too_long() {
        use crate::types::DEFAULT_SUMMARY_MAX_CHARS as MAX;
        // Default bound is 200, so the historical 101 is now valid.
        assert!(validate_summary(&"a".repeat(101), MAX).is_ok());
        assert!(validate_summary(&"a".repeat(MAX + 1), MAX).is_err());
        // The bound is configurable — a smaller limit rejects sooner.
        assert!(validate_summary(&"a".repeat(101), 100).is_err());
        // Measured in characters, not bytes: 200 multibyte chars fit.
        assert!(validate_summary(&"é".repeat(MAX), MAX).is_ok());
        assert!(validate_summary(&"é".repeat(MAX + 1), MAX).is_err());
    }

    #[test]
    fn test_validate_summary_accepts_valid() {
        use crate::types::DEFAULT_SUMMARY_MAX_CHARS as MAX;
        assert!(validate_summary("Short summary", MAX).is_ok());
        assert!(validate_summary(&"a".repeat(MAX), MAX).is_ok());
        assert!(validate_summary("x", MAX).is_ok());
    }

    #[tokio::test]
    async fn test_create_memory_fails_with_empty_summary() {
        let (_temp, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        params.summary = "".to_string();
        let result = create_memory(&store, params, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[tokio::test]
    async fn test_create_memory_accepts_summary_over_legacy_100_limit() {
        // The default bound was raised from 100 to 200 — a 150-char summary
        // that create used to reject must now succeed.
        let (_temp, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        params.summary = "a".repeat(150);
        let result = create_memory(&store, params, None).await;
        assert!(result.is_ok(), "150-char summary should be accepted now");
    }

    #[tokio::test]
    async fn test_create_memory_fails_with_too_long_summary() {
        let (_temp, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        // Past the raised default (200) — still rejected.
        params.summary = "a".repeat(crate::types::DEFAULT_SUMMARY_MAX_CHARS + 1);
        let result = create_memory(&store, params, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("200"));
    }

    #[tokio::test]
    async fn test_create_memory_with_explicit_title() {
        let (_temp, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        params.title = Some("My Custom Title".to_string());
        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();
        assert_eq!(memory.title, Some("My Custom Title".to_string()));
    }

    #[tokio::test]
    async fn test_create_memory_without_title() {
        let (_temp, store) = setup_test_store().await;
        let params = minimal_create_params();
        let result = create_memory(&store, params, None).await.unwrap();
        let memory = store.get(&result.id).await.unwrap();
        assert_eq!(memory.title, None);
    }

    #[tokio::test]
    async fn test_create_memory_title_in_filename() {
        let (_temp, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        params.title = Some("Database Choice".to_string());
        let result = create_memory(&store, params, None).await.unwrap();

        // Verify the file on disk has the slug in its name
        let memories_dir = _temp.path().join(".engramdb").join("memories");
        let mut found = false;
        for entry in std::fs::read_dir(&memories_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(&result.id) && name.starts_with("database-choice_") {
                found = true;
            }
        }
        assert!(found, "Expected file with slug prefix 'database-choice_'");
    }
}

#[cfg(test)]
mod epistemic_create_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Epistemic, Generality, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    async fn setup() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    fn params(type_: MemoryType) -> CreateParams {
        CreateParams {
            type_,
            content: "content".into(),
            summary: "summary".into(),
            title: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Shared,
            provenance: Provenance::human(),
            supersedes: vec![],
            audience: None,
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
        }
    }

    #[tokio::test]
    async fn epistemic_defaults_from_type_and_diagonal_decay_unchanged() {
        let (_t, store) = setup().await;
        let result = create_memory(&store, params(MemoryType::Debug), None)
            .await
            .unwrap();
        let m = store.get(&result.id).await.unwrap();
        assert_eq!(m.epistemic, Epistemic::Observation);
        // Diagonal ⇒ the type default decay (Debug: exponential 30d).
        let decay = m.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential);
        assert_eq!(decay.half_life, Some(Duration::days(30)));
        assert_eq!(m.valid_while, None);
    }

    #[tokio::test]
    async fn off_diagonal_observation_gets_observation_decay() {
        let (_t, store) = setup().await;
        // Hazard defaults to Fact; declare an Observation → 90d/0.2 curve
        // (built-in constants; no engine ⇒ no config override).
        let mut p = params(MemoryType::Hazard);
        p.epistemic = Some(Epistemic::Observation);
        let result = create_memory(&store, p, None).await.unwrap();
        let m = store.get(&result.id).await.unwrap();
        assert_eq!(m.epistemic, Epistemic::Observation);
        let decay = m.decay.unwrap();
        assert_eq!(decay.half_life, Some(Duration::days(90)));
        assert_eq!(decay.floor, 0.2);
    }

    #[tokio::test]
    async fn off_diagonal_fact_never_fades_and_explicit_decay_wins() {
        let (_t, store) = setup().await;
        // Debug (Observation default) declared a Fact → Decay::none().
        let mut p = params(MemoryType::Debug);
        p.epistemic = Some(Epistemic::Fact);
        let result = create_memory(&store, p, None).await.unwrap();
        let m = store.get(&result.id).await.unwrap();
        let decay = m.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::None);
        assert_eq!(decay.floor, 0.0);

        // Explicit user decay wins over the off-diagonal default.
        let mut p = params(MemoryType::Debug);
        p.epistemic = Some(Epistemic::Fact);
        p.decay_strategy = Some("linear".into());
        p.decay_ttl = Some(86400);
        let result = create_memory(&store, p, None).await.unwrap();
        let m = store.get(&result.id).await.unwrap();
        assert_eq!(m.decay.unwrap().strategy, DecayStrategy::Linear);
    }

    #[tokio::test]
    async fn validity_assembled_and_empty_stays_none() {
        let (_t, store) = setup().await;
        let mut p = params(MemoryType::Decision);
        p.premise = Some("while ort is pinned".into());
        p.invalidated_by = vec!["Cargo.lock".into()];
        p.origin_task = Some("epistemic".into());
        p.generality = Some(Generality::Task);
        p.valid_from = Some("2026-01-01T00:00:00Z".parse().unwrap());
        let result = create_memory(&store, p, None).await.unwrap();
        let m = store.get(&result.id).await.unwrap();
        let v = m.valid_while.unwrap();
        assert_eq!(v.premise.as_deref(), Some("while ort is pinned"));
        assert_eq!(v.invalidated_by, vec!["Cargo.lock".to_string()]);
        assert_eq!(v.origin_task.as_deref(), Some("epistemic"));
        assert_eq!(v.generality, Generality::Task);
        assert_eq!(m.valid_from, Some("2026-01-01T00:00:00Z".parse().unwrap()));

        // All-empty validity params ⇒ valid_while stays None.
        let result = create_memory(&store, params(MemoryType::Decision), None)
            .await
            .unwrap();
        assert_eq!(store.get(&result.id).await.unwrap().valid_while, None);
    }

    #[tokio::test]
    async fn create_with_supersedes_closes_old_window() {
        let (_t, store) = setup().await;
        let old = create_memory(&store, params(MemoryType::Decision), None)
            .await
            .unwrap();

        let mut p = params(MemoryType::Decision);
        p.supersedes = vec![old.id.clone(), "missing-id".into()];
        let new = create_memory(&store, p, None).await.unwrap();

        let old_mem = store.get(&old.id).await.unwrap();
        assert!(old_mem.invalidated_at.is_some(), "window closed");
        assert_eq!(old_mem.superseded_by.as_deref(), Some(new.id.as_str()));
        // The missing id was skipped without failing the create.
        let new_mem = store.get(&new.id).await.unwrap();
        assert!(new_mem.supersedes.contains(&"missing-id".to_string()));
    }
}
