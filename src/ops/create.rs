//! Memory creation operation.

use crate::ops::parse_decay_strategy;
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use crate::types::{Decay, DecayStrategy, Memory, MemoryType, Provenance, Visibility};
use anyhow::Result;
use chrono::Duration;

/// Parameters for creating a new memory.
pub struct CreateParams {
    pub type_: MemoryType,
    pub content: String,
    pub summary: String,
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
}

/// Result of a create operation.
#[derive(Debug)]
pub struct CreateResult {
    pub id: String,
    pub summary: String,
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

    let id = store.create(&memory).await?;

    // Embed the newly created memory if an engine with embeddings is available
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            let saved = store.get(&id).await?;
            engine.embed_memory(&saved).await?;

            // Detect contradictions with existing memories (best-effort, background).
            // Vector search + NLI classification run inline (needs &engine), but
            // the resulting challenge writes are spawned so create_memory returns
            // without waiting for them.
            if engine.nli_available() {
                if let Ok(contradictions) = engine.detect_contradictions(&saved).await {
                    if !contradictions.is_empty() {
                        let store_clone = store.clone();
                        tokio::spawn(async move {
                            for (existing_id, nli_result) in &contradictions {
                                let evidence = format!(
                                    "NLI contradiction detected (score: {:.2}): new memory '{}' contradicts this memory",
                                    nli_result.contradiction, saved.summary
                                );
                                let _ = crate::ops::challenge::challenge_memory(
                                    &store_clone,
                                    existing_id,
                                    &evidence,
                                    None,
                                )
                                .await;
                            }
                        });
                    }
                }
            }
        }
    }

    Ok(CreateResult {
        id,
        summary: memory.summary,
    })
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
        }
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
}
