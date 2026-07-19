//! Update memory operation.

use crate::ops::parse_decay_strategy;
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use crate::types::{
    Decay, DecayStrategy, Epistemic, Generality, MemoryType, MemoryUpdate, Status, Validity,
    Visibility,
};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

/// Parameters for updating a memory.
///
/// All fields are optional; only provided fields will be updated.
pub struct UpdateParams {
    pub type_: Option<MemoryType>,
    pub content: Option<String>,
    pub summary: Option<String>,
    pub title: Option<String>,
    pub physical: Option<Vec<String>>,
    pub logical: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub tags_add: Option<Vec<String>>,
    pub tags_remove: Option<Vec<String>>,
    pub criticality: Option<f64>,
    pub confidence: Option<f64>,
    pub details: Option<String>,
    pub visibility: Option<Visibility>,
    pub status: Option<Status>,
    pub supersedes: Option<Vec<String>>,
    /// Reclassify the memory's epistemic class.
    pub epistemic: Option<Epistemic>,
    /// Validity-condition edits, merged into the existing `valid_while`
    /// (fields not provided keep their current values).
    pub premise: Option<String>,
    pub invalidated_by: Option<Vec<String>>,
    pub origin_task: Option<String>,
    pub generality: Option<Generality>,
    /// Valid-time start (backdating).
    pub valid_from: Option<DateTime<Utc>>,
    /// Clear the whole `valid_while` condition.
    pub clear_validity: bool,
    /// Reopen a closed validity window (§2.4): clears `invalidated_at` and
    /// `superseded_by`.
    pub clear_invalidated: bool,
    pub decay_strategy: Option<String>,
    pub decay_half_life: Option<u64>,
    pub decay_ttl: Option<u64>,
    pub decay_floor: Option<f64>,
    /// When `true`, run re-embedding + contradiction detection in a background
    /// `tokio::spawn`ed task and return immediately. Errors in the background
    /// task are logged via `tracing` and never surface to the caller. Used by
    /// the MCP `update` tool so the agent isn't blocked on embedding inference.
    ///
    /// When `false` (default), re-embedding runs inline before the function
    /// returns; contradiction detection is not performed (matches the
    /// pre-async-flag CLI behavior).
    pub embed_async: bool,
}

/// Update an existing memory.
///
/// If `engine` is provided and has embeddings available, the memory is
/// re-embedded into the vector store after the update succeeds.
pub async fn update_memory(
    store: &MemoryStore,
    id: &str,
    params: UpdateParams,
    engine: Option<&RetrievalEngine>,
) -> Result<bool> {
    // Validate score fields in the shared core so every front-end path is
    // covered (mirrors create_memory — see finding #3 there): a new caller
    // must not be able to persist NaN/out-of-range scores that skew ranking.
    if let Some(criticality) = params.criticality {
        super::validate_score(criticality, "criticality")?;
    }
    if let Some(confidence) = params.confidence {
        super::validate_score(confidence, "confidence")?;
    }
    if let Some(floor) = params.decay_floor {
        super::validate_score(floor, "decay_floor")?;
    }

    // Parse the decay strategy up front (it doesn't depend on the stored
    // memory) so invalid input fails before taking the write lock.
    let parsed_decay_strategy = match params.decay_strategy.as_deref() {
        Some(strategy_str) => Some(parse_decay_strategy(strategy_str)?),
        None => None,
    };
    let wants_decay_update = parsed_decay_strategy.is_some()
        || params.decay_half_life.is_some()
        || params.decay_ttl.is_some()
        || params.decay_floor.is_some();
    let embed_async = params.embed_async;
    // Keep a copy for the post-update supersession pass (§2.4 writer 1) —
    // the list itself is moved into the update closure below.
    let supersedes_for_close = params.supersedes.clone();

    // Merge the params into the memory atomically: `update_with` re-reads the
    // memory inside the per-project write lock, so two concurrent updates
    // cannot snapshot the same old state and silently erase each other's
    // changes (the old get-merge-update flow did exactly that).
    let saved = store
        .update_with(id, move |memory| {
            // Apply direct field updates
            let mut update = MemoryUpdate::new();
            update.type_ = params.type_;
            update.content = params.content;
            update.summary = params.summary;
            update.title = params.title;
            update.details = params.details;
            update.physical = params.physical;
            update.logical = params.logical;
            update.criticality = params.criticality;
            update.confidence = params.confidence;
            update.visibility = params.visibility;
            update.status = params.status;
            update.supersedes = params.supersedes;
            // Reclassifying `epistemic` deliberately does NOT touch decay:
            // update never changes decay unless the caller passes decay
            // fields explicitly (unlike create, which derives a class-default
            // curve). Pass --decay-* alongside --epistemic to re-curve.
            update.epistemic = params.epistemic;
            update.valid_from = params.valid_from;
            update.clear_invalidated = params.clear_invalidated;

            // Validity edits: merge the provided fields into the existing
            // condition (or a fresh default), then let `apply_to`'s
            // empty-Validity normalization decide whether anything persists.
            // `clear_validity` wins over piecemeal edits in the same call.
            if params.clear_validity {
                update.valid_while = Some(Validity::default()); // all-empty ⇒ cleared
            } else if params.premise.is_some()
                || params.invalidated_by.is_some()
                || params.origin_task.is_some()
                || params.generality.is_some()
            {
                let mut validity = memory.valid_while.clone().unwrap_or_default();
                if let Some(premise) = params.premise {
                    validity.premise = Some(premise);
                }
                if let Some(invalidated_by) = params.invalidated_by {
                    validity.invalidated_by = invalidated_by;
                }
                if let Some(origin_task) = params.origin_task {
                    validity.origin_task = Some(origin_task);
                }
                if let Some(generality) = params.generality {
                    validity.generality = generality;
                }
                update.valid_while = Some(validity);
            }

            // Handle tags: full replacement first if provided
            if let Some(tags) = params.tags {
                update.tags = Some(tags);
            }

            // Handle decay config updates (merged against the locked state)
            if wants_decay_update {
                let existing_decay = memory.decay.clone();

                let strategy = parsed_decay_strategy.unwrap_or_else(|| {
                    // Keep existing strategy or default to None
                    existing_decay
                        .as_ref()
                        .map(|d| d.strategy.clone())
                        .unwrap_or(DecayStrategy::None)
                });

                let mut decay = Decay::new(strategy);

                // Merge numeric fields: prefer params, fall back to existing
                if let Some(half_life_secs) = params.decay_half_life {
                    decay.half_life = Some(Duration::seconds(half_life_secs as i64));
                } else if let Some(existing) = &existing_decay {
                    decay.half_life = existing.half_life;
                }

                if let Some(ttl_secs) = params.decay_ttl {
                    decay.ttl = Some(Duration::seconds(ttl_secs as i64));
                } else if let Some(existing) = &existing_decay {
                    decay.ttl = existing.ttl;
                }

                if let Some(floor) = params.decay_floor {
                    decay.floor = floor;
                } else if let Some(existing) = &existing_decay {
                    decay.floor = existing.floor;
                }

                update.decay = Some(decay);
            }

            // Apply the update to get the current state
            update.apply_to(memory);

            // Then handle tag additions (after replacement)
            if let Some(tags_to_add) = params.tags_add {
                memory.tags.extend(tags_to_add);
                // Deduplicate tags
                memory.tags.sort();
                memory.tags.dedup();
            }

            // Then handle tag removals
            if let Some(tags_to_remove) = params.tags_remove {
                memory.tags.retain(|tag| !tags_to_remove.contains(tag));
            }

            Ok(())
        })
        .await?;

    // Supersession via update closes windows exactly like create (§2.4
    // writer 1). Ids already invalidated (e.g. listed on a previous update)
    // are skipped inside the helper, so re-sending the same list is a no-op.
    if let Some(ref supersedes) = supersedes_for_close {
        super::close_superseded_windows(store, supersedes, &saved.id).await;
    }

    // Re-embed the updated memory if an engine with embeddings is available.
    //
    // - `embed_async = true`  (MCP path): spawn embed + NLI contradiction
    //                                     detection in the background. An edit
    //                                     can introduce a new contradiction
    //                                     against existing memories, so the
    //                                     async path opportunistically writes
    //                                     challenges. Vector search may briefly
    //                                     not reflect the edit until the task
    //                                     finishes.
    // - `embed_async = false` (CLI / tests): re-embed inline; no NLI pass
    //                                        (preserves the pre-flag behavior).
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            if embed_async {
                let _ = engine.spawn_ingest(saved);
            } else {
                engine.embed_memory(&saved).await?;
            }
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    async fn setup_test_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        (temp_dir, store)
    }

    async fn create_test_memory(store: &MemoryStore) -> String {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );
        memory.tags = vec!["tag1".to_string(), "tag2".to_string()];
        store.create(&memory).await.unwrap()
    }

    fn empty_update_params() -> UpdateParams {
        UpdateParams {
            type_: None,
            content: None,
            summary: None,
            title: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            details: None,
            visibility: None,
            status: None,
            supersedes: None,
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: false,
            clear_invalidated: false,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            embed_async: false,
        }
    }

    #[tokio::test]
    async fn test_update_tags_add_appends_and_deduplicates() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Add new tags, including one duplicate
        let mut params = empty_update_params();
        params.tags_add = Some(vec!["tag2".to_string(), "tag3".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.tags.len(), 3);
        assert!(memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
        assert!(memory.tags.contains(&"tag3".to_string()));
    }

    #[tokio::test]
    async fn test_update_tags_remove_filters_correctly() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Remove one tag
        let mut params = empty_update_params();
        params.tags_remove = Some(vec!["tag1".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.tags.len(), 1);
        assert!(!memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
    }

    #[tokio::test]
    async fn test_update_tags_remove_multiple() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Remove both tags
        let mut params = empty_update_params();
        params.tags_remove = Some(vec!["tag1".to_string(), "tag2".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.tags.len(), 0);
    }

    #[tokio::test]
    async fn test_update_tags_combined_replace_then_add() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Replace tags with new set, then add more
        let mut params = empty_update_params();
        params.tags = Some(vec!["new1".to_string(), "new2".to_string()]);
        params.tags_add = Some(vec!["new2".to_string(), "new3".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.tags.len(), 3);
        assert!(memory.tags.contains(&"new1".to_string()));
        assert!(memory.tags.contains(&"new2".to_string()));
        assert!(memory.tags.contains(&"new3".to_string()));
        assert!(!memory.tags.contains(&"tag1".to_string()));
        assert!(!memory.tags.contains(&"tag2".to_string()));
    }

    #[tokio::test]
    async fn test_update_supersedes_persists() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Set supersedes
        let mut params = empty_update_params();
        params.supersedes = Some(vec!["old-id-1".to_string(), "old-id-2".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.supersedes.len(), 2);
        assert!(memory.supersedes.contains(&"old-id-1".to_string()));
        assert!(memory.supersedes.contains(&"old-id-2".to_string()));
    }

    #[tokio::test]
    async fn test_update_supersedes_roundtrip() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Set supersedes
        let mut params = empty_update_params();
        params.supersedes = Some(vec!["prev-memory".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        // Reload from disk
        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.supersedes, vec!["prev-memory".to_string()]);
    }

    #[tokio::test]
    async fn test_supersedes_defaults_to_empty_vec() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Get memory without setting supersedes
        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.supersedes.len(), 0);
    }

    #[tokio::test]
    async fn test_update_tags_add_to_empty() {
        let (_temp, store) = setup_test_store().await;
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        memory.tags = vec![];
        let id = store.create(&memory).await.unwrap();

        let mut params = empty_update_params();
        params.tags_add = Some(vec!["first".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.tags.len(), 1);
        assert_eq!(memory.tags[0], "first");
    }

    #[tokio::test]
    async fn test_update_tags_remove_nonexistent() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Try to remove a tag that doesn't exist
        let mut params = empty_update_params();
        params.tags_remove = Some(vec!["nonexistent".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        // Should still have original tags
        assert_eq!(memory.tags.len(), 2);
        assert!(memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
    }

    #[tokio::test]
    async fn test_update_all_tag_operations_combined() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Replace with ["a", "b"], add ["b", "c"], remove ["a"]
        // Final result should be ["b", "c"]
        let mut params = empty_update_params();
        params.tags = Some(vec!["a".to_string(), "b".to_string()]);
        params.tags_add = Some(vec!["b".to_string(), "c".to_string()]);
        params.tags_remove = Some(vec!["a".to_string()]);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.tags.len(), 2);
        assert!(memory.tags.contains(&"b".to_string()));
        assert!(memory.tags.contains(&"c".to_string()));
        assert!(!memory.tags.contains(&"a".to_string()));
    }

    /// Two concurrent `update_memory` calls must both persist their changes.
    /// The merge logic runs inside `MemoryStore::update_with` (re-read under
    /// the per-project write lock), so neither call can snapshot stale state
    /// and silently erase the other's tag.
    #[tokio::test]
    async fn test_concurrent_updates_both_persist() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        let store_a = store.clone();
        let id_a = id.clone();
        let task_a = tokio::spawn(async move {
            let mut params = empty_update_params();
            params.tags_add = Some(vec!["concurrent-a".to_string()]);
            update_memory(&store_a, &id_a, params, None).await.unwrap();
        });

        let store_b = store.clone();
        let id_b = id.clone();
        let task_b = tokio::spawn(async move {
            let mut params = empty_update_params();
            params.tags_add = Some(vec!["concurrent-b".to_string()]);
            update_memory(&store_b, &id_b, params, None).await.unwrap();
        });

        task_a.await.unwrap();
        task_b.await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert!(
            memory.tags.contains(&"concurrent-a".to_string()),
            "tag from update A was lost: {:?}",
            memory.tags
        );
        assert!(
            memory.tags.contains(&"concurrent-b".to_string()),
            "tag from update B was lost: {:?}",
            memory.tags
        );
        // Original tags survive too
        assert!(memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
    }

    #[tokio::test]
    async fn test_update_decay_config_on_existing_memory() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Update decay config
        let mut params = empty_update_params();
        params.decay_strategy = Some("exponential".to_string());
        params.decay_half_life = Some(604800); // 7 days
        params.decay_floor = Some(0.4);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential);
        assert_eq!(decay.half_life, Some(Duration::seconds(604800)));
        assert_eq!(decay.floor, 0.4);
    }

    #[tokio::test]
    async fn test_update_decay_partial_fields() {
        let (_temp, store) = setup_test_store().await;
        let memory = Memory::new(MemoryType::Intent, "Test", "Content", Provenance::human());
        // Intent has default exponential decay with 14 days half-life
        let id = store.create(&memory).await.unwrap();

        // Update only the floor, should keep existing strategy
        let mut params = empty_update_params();
        params.decay_floor = Some(0.25);

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential); // Kept from original
        assert_eq!(decay.half_life, Some(Duration::days(14))); // Kept from original
        assert_eq!(decay.floor, 0.25); // Updated
    }

    /// Score validation must live in the ops core, not only in the
    /// front-ends: out-of-range or non-finite values are rejected before the
    /// write lock, and the stored memory is untouched.
    #[tokio::test]
    async fn test_update_rejects_invalid_scores_in_core() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        for (field, params) in [
            ("criticality", {
                let mut p = empty_update_params();
                p.criticality = Some(5.0);
                p
            }),
            ("confidence", {
                let mut p = empty_update_params();
                p.confidence = Some(-0.1);
                p
            }),
            ("decay_floor", {
                let mut p = empty_update_params();
                p.decay_floor = Some(f64::NAN);
                p
            }),
        ] {
            let err = update_memory(&store, &id, params, None)
                .await
                .expect_err(&format!("invalid {field} must be rejected"));
            assert!(
                err.to_string().contains(field),
                "error should name the offending field {field}: {err}"
            );
        }

        // The memory is unchanged.
        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.criticality, 0.5);
    }

    #[tokio::test]
    async fn test_update_decay_invalid_strategy_returns_error() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        let mut params = empty_update_params();
        params.decay_strategy = Some("invalid".to_string());

        let result = update_memory(&store, &id, params, None).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid decay strategy"));
    }

    #[tokio::test]
    async fn test_update_decay_change_strategy_preserve_other_fields() {
        let (_temp, store) = setup_test_store().await;
        let memory = Memory::new(MemoryType::Debug, "Test", "Content", Provenance::human());
        // Debug has default exponential with 30 days half-life
        let id = store.create(&memory).await.unwrap();

        // Change only strategy to linear
        let mut params = empty_update_params();
        params.decay_strategy = Some("linear".to_string());

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Linear);
        // Half-life should be preserved from original
        assert_eq!(decay.half_life, Some(Duration::days(30)));
    }

    #[tokio::test]
    async fn test_update_decay_none_strategy() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        // Set decay to none
        let mut params = empty_update_params();
        params.decay_strategy = Some("none".to_string());

        update_memory(&store, &id, params, None).await.unwrap();

        let memory = store.get(&id).await.unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::None);
    }
}

#[cfg(test)]
mod epistemic_update_tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Epistemic, Generality, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    async fn setup() -> (TempDir, MemoryStore, String) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let m = Memory::new(MemoryType::Decision, "S", "C", Provenance::human());
        let id = store.create(&m).await.unwrap();
        (tmp, store, id)
    }

    fn base() -> UpdateParams {
        UpdateParams {
            type_: None,
            content: None,
            summary: None,
            title: None,
            physical: None,
            logical: None,
            tags: None,
            tags_add: None,
            tags_remove: None,
            criticality: None,
            confidence: None,
            details: None,
            visibility: None,
            status: None,
            supersedes: None,
            epistemic: None,
            premise: None,
            invalidated_by: None,
            origin_task: None,
            generality: None,
            valid_from: None,
            clear_validity: false,
            clear_invalidated: false,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            embed_async: false,
        }
    }

    #[tokio::test]
    async fn update_merges_validity_fields() {
        let (_t, store, id) = setup().await;

        let mut p = base();
        p.premise = Some("premise A".into());
        update_memory(&store, &id, p, None).await.unwrap();

        // Second edit adds a field without erasing the first.
        let mut p = base();
        p.origin_task = Some("task-x".into());
        p.generality = Some(Generality::Task);
        update_memory(&store, &id, p, None).await.unwrap();

        let m = store.get(&id).await.unwrap();
        let v = m.valid_while.unwrap();
        assert_eq!(v.premise.as_deref(), Some("premise A"));
        assert_eq!(v.origin_task.as_deref(), Some("task-x"));
        assert_eq!(v.generality, Generality::Task);

        // clear_validity wipes the whole condition.
        let mut p = base();
        p.clear_validity = true;
        update_memory(&store, &id, p, None).await.unwrap();
        assert_eq!(store.get(&id).await.unwrap().valid_while, None);
    }

    #[tokio::test]
    async fn update_reclassifies_epistemic() {
        let (_t, store, id) = setup().await;
        let mut p = base();
        p.epistemic = Some(Epistemic::Observation);
        update_memory(&store, &id, p, None).await.unwrap();
        assert_eq!(
            store.get(&id).await.unwrap().epistemic,
            Epistemic::Observation
        );
    }

    #[tokio::test]
    async fn update_supersedes_closes_windows_and_reopen_clears() {
        let (_t, store, id) = setup().await;
        let old = Memory::new(MemoryType::Decision, "Old", "C", Provenance::human());
        let old_id = store.create(&old).await.unwrap();

        let mut p = base();
        p.supersedes = Some(vec![old_id.clone()]);
        update_memory(&store, &id, p, None).await.unwrap();

        let old_mem = store.get(&old_id).await.unwrap();
        assert!(old_mem.invalidated_at.is_some());
        assert_eq!(old_mem.superseded_by.as_deref(), Some(id.as_str()));

        // Reopening (§2.4): clear_invalidated restores the closed window.
        let mut p = base();
        p.clear_invalidated = true;
        update_memory(&store, &old_id, p, None).await.unwrap();
        let old_mem = store.get(&old_id).await.unwrap();
        assert_eq!(old_mem.invalidated_at, None);
        assert_eq!(old_mem.superseded_by, None);
    }
}
