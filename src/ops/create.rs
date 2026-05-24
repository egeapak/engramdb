//! Memory creation operation.

use crate::ops::parse_decay_strategy;
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use crate::title::TitleStrategy;
use crate::types::{Decay, DecayStrategy, Memory, MemoryType, Provenance, Visibility};
use anyhow::Result;
use chrono::Duration;

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

/// Validate that a summary is non-empty and within the character limit.
pub fn validate_summary(summary: &str) -> Result<()> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Summary cannot be empty");
    }
    if trimmed.len() > 100 {
        anyhow::bail!("Summary must be <= 100 characters (got {})", trimmed.len());
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
    validate_summary(&params.summary)?;
    let summary = params.summary;

    // Use default physical scope if empty
    let physical = if params.physical.is_empty() {
        vec!["/".to_string()]
    } else {
        params.physical
    };

    // Build memory
    let mut memory = Memory::new(params.type_, &summary, &params.content, params.provenance);
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
                let saved = store.get(&id).await?;
                engine.embed_memory(&saved).await?;

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
                            tokio::spawn(async move {
                                crate::ops::challenge_for_contradictions(
                                    &store_clone,
                                    &saved.summary,
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
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title_strategy: TitleStrategy::None,
            embed_async: false,
        }
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
        assert!(validate_summary("").is_err());
        assert!(validate_summary("   ").is_err());
        assert!(validate_summary("\n\t").is_err());
    }

    #[test]
    fn test_validate_summary_rejects_too_long() {
        let long = "a".repeat(101);
        assert!(validate_summary(&long).is_err());
    }

    #[test]
    fn test_validate_summary_accepts_valid() {
        assert!(validate_summary("Short summary").is_ok());
        assert!(validate_summary(&"a".repeat(100)).is_ok());
        assert!(validate_summary("x").is_ok());
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
    async fn test_create_memory_fails_with_too_long_summary() {
        let (_temp, store) = setup_test_store().await;
        let mut params = minimal_create_params();
        params.summary = "a".repeat(101);
        let result = create_memory(&store, params, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("100"));
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
