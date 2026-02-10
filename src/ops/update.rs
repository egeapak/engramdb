//! Update memory operation.

use crate::ops::parse_decay_strategy;
use crate::storage::MemoryStore;
use crate::types::{Decay, DecayStrategy, MemoryType, MemoryUpdate, Status, Visibility};
use anyhow::Result;
use chrono::Duration;

/// Parameters for updating a memory.
///
/// All fields are optional; only provided fields will be updated.
pub struct UpdateParams {
    pub type_: Option<MemoryType>,
    pub content: Option<String>,
    pub summary: Option<String>,
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
    pub decay_strategy: Option<String>,
    pub decay_half_life: Option<u64>,
    pub decay_ttl: Option<u64>,
    pub decay_floor: Option<f64>,
}

/// Update an existing memory.
pub fn update_memory(store: &MemoryStore, id: &str, params: UpdateParams) -> Result<bool> {
    // Load the existing memory to handle tag operations
    let mut memory = store.get(id)?;

    // Apply direct field updates
    let mut update = MemoryUpdate::new();
    update.type_ = params.type_;
    update.content = params.content;
    update.summary = params.summary;
    update.details = params.details;
    update.physical = params.physical;
    update.logical = params.logical;
    update.criticality = params.criticality;
    update.confidence = params.confidence;
    update.visibility = params.visibility;
    update.status = params.status;
    update.supersedes = params.supersedes;

    // Handle tags: full replacement first if provided
    if let Some(tags) = params.tags {
        update.tags = Some(tags);
    }

    // Handle decay config updates
    if params.decay_strategy.is_some()
        || params.decay_half_life.is_some()
        || params.decay_ttl.is_some()
        || params.decay_floor.is_some()
    {
        // Get existing decay or create new one
        let existing_decay = memory.decay.clone();

        let strategy = if let Some(ref strategy_str) = params.decay_strategy {
            parse_decay_strategy(strategy_str)?
        } else {
            // Keep existing strategy or default to None
            existing_decay
                .as_ref()
                .map(|d| d.strategy.clone())
                .unwrap_or(DecayStrategy::None)
        };

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
    update.apply_to(&mut memory);

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

    // Convert back to update with final tags state
    let mut final_update = MemoryUpdate::new();
    final_update.type_ = Some(memory.type_);
    final_update.content = Some(memory.content);
    final_update.summary = Some(memory.summary);
    final_update.details = memory.details;
    final_update.physical = Some(memory.physical);
    final_update.logical = Some(memory.logical);
    final_update.tags = Some(memory.tags);
    final_update.criticality = Some(memory.criticality);
    final_update.confidence = Some(memory.confidence);
    final_update.visibility = Some(memory.visibility);
    final_update.status = Some(memory.status);
    final_update.supersedes = Some(memory.supersedes);
    final_update.decay = memory.decay;

    store.update(id, final_update)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStore;
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn setup_test_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();
        (temp_dir, store)
    }

    fn create_test_memory(store: &MemoryStore) -> String {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );
        memory.tags = vec!["tag1".to_string(), "tag2".to_string()];
        store.create(&memory).unwrap()
    }

    fn empty_update_params() -> UpdateParams {
        UpdateParams {
            type_: None,
            content: None,
            summary: None,
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
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
        }
    }

    #[test]
    fn test_update_tags_add_appends_and_deduplicates() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Add new tags, including one duplicate
        let mut params = empty_update_params();
        params.tags_add = Some(vec!["tag2".to_string(), "tag3".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.tags.len(), 3);
        assert!(memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
        assert!(memory.tags.contains(&"tag3".to_string()));
    }

    #[test]
    fn test_update_tags_remove_filters_correctly() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Remove one tag
        let mut params = empty_update_params();
        params.tags_remove = Some(vec!["tag1".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.tags.len(), 1);
        assert!(!memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
    }

    #[test]
    fn test_update_tags_remove_multiple() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Remove both tags
        let mut params = empty_update_params();
        params.tags_remove = Some(vec!["tag1".to_string(), "tag2".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.tags.len(), 0);
    }

    #[test]
    fn test_update_tags_combined_replace_then_add() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Replace tags with new set, then add more
        let mut params = empty_update_params();
        params.tags = Some(vec!["new1".to_string(), "new2".to_string()]);
        params.tags_add = Some(vec!["new2".to_string(), "new3".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.tags.len(), 3);
        assert!(memory.tags.contains(&"new1".to_string()));
        assert!(memory.tags.contains(&"new2".to_string()));
        assert!(memory.tags.contains(&"new3".to_string()));
        assert!(!memory.tags.contains(&"tag1".to_string()));
        assert!(!memory.tags.contains(&"tag2".to_string()));
    }

    #[test]
    fn test_update_supersedes_persists() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Set supersedes
        let mut params = empty_update_params();
        params.supersedes = Some(vec!["old-id-1".to_string(), "old-id-2".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.supersedes.len(), 2);
        assert!(memory.supersedes.contains(&"old-id-1".to_string()));
        assert!(memory.supersedes.contains(&"old-id-2".to_string()));
    }

    #[test]
    fn test_update_supersedes_roundtrip() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Set supersedes
        let mut params = empty_update_params();
        params.supersedes = Some(vec!["prev-memory".to_string()]);

        update_memory(&store, &id, params).unwrap();

        // Reload from disk
        let memory = store.get(&id).unwrap();
        assert_eq!(memory.supersedes, vec!["prev-memory".to_string()]);
    }

    #[test]
    fn test_supersedes_defaults_to_empty_vec() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Get memory without setting supersedes
        let memory = store.get(&id).unwrap();
        assert_eq!(memory.supersedes.len(), 0);
    }

    #[test]
    fn test_update_tags_add_to_empty() {
        let (_temp, store) = setup_test_store();
        let mut memory = Memory::new(MemoryType::Decision, "Test", "Content", Provenance::human());
        memory.tags = vec![];
        let id = store.create(&memory).unwrap();

        let mut params = empty_update_params();
        params.tags_add = Some(vec!["first".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.tags.len(), 1);
        assert_eq!(memory.tags[0], "first");
    }

    #[test]
    fn test_update_tags_remove_nonexistent() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Try to remove a tag that doesn't exist
        let mut params = empty_update_params();
        params.tags_remove = Some(vec!["nonexistent".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        // Should still have original tags
        assert_eq!(memory.tags.len(), 2);
        assert!(memory.tags.contains(&"tag1".to_string()));
        assert!(memory.tags.contains(&"tag2".to_string()));
    }

    #[test]
    fn test_update_all_tag_operations_combined() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Replace with ["a", "b"], add ["b", "c"], remove ["a"]
        // Final result should be ["b", "c"]
        let mut params = empty_update_params();
        params.tags = Some(vec!["a".to_string(), "b".to_string()]);
        params.tags_add = Some(vec!["b".to_string(), "c".to_string()]);
        params.tags_remove = Some(vec!["a".to_string()]);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert_eq!(memory.tags.len(), 2);
        assert!(memory.tags.contains(&"b".to_string()));
        assert!(memory.tags.contains(&"c".to_string()));
        assert!(!memory.tags.contains(&"a".to_string()));
    }

    #[test]
    fn test_update_decay_config_on_existing_memory() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Update decay config
        let mut params = empty_update_params();
        params.decay_strategy = Some("exponential".to_string());
        params.decay_half_life = Some(604800); // 7 days
        params.decay_floor = Some(0.4);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential);
        assert_eq!(decay.half_life, Some(Duration::seconds(604800)));
        assert_eq!(decay.floor, 0.4);
    }

    #[test]
    fn test_update_decay_partial_fields() {
        let (_temp, store) = setup_test_store();
        let memory = Memory::new(MemoryType::Intent, "Test", "Content", Provenance::human());
        // Intent has default exponential decay with 14 days half-life
        let id = store.create(&memory).unwrap();

        // Update only the floor, should keep existing strategy
        let mut params = empty_update_params();
        params.decay_floor = Some(0.25);

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Exponential); // Kept from original
        assert_eq!(decay.half_life, Some(Duration::days(14))); // Kept from original
        assert_eq!(decay.floor, 0.25); // Updated
    }

    #[test]
    fn test_update_decay_invalid_strategy_returns_error() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        let mut params = empty_update_params();
        params.decay_strategy = Some("invalid".to_string());

        let result = update_memory(&store, &id, params);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid decay strategy"));
    }

    #[test]
    fn test_update_decay_change_strategy_preserve_other_fields() {
        let (_temp, store) = setup_test_store();
        let memory = Memory::new(MemoryType::Debug, "Test", "Content", Provenance::human());
        // Debug has default exponential with 30 days half-life
        let id = store.create(&memory).unwrap();

        // Change only strategy to linear
        let mut params = empty_update_params();
        params.decay_strategy = Some("linear".to_string());

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::Linear);
        // Half-life should be preserved from original
        assert_eq!(decay.half_life, Some(Duration::days(30)));
    }

    #[test]
    fn test_update_decay_none_strategy() {
        let (_temp, store) = setup_test_store();
        let id = create_test_memory(&store);

        // Set decay to none
        let mut params = empty_update_params();
        params.decay_strategy = Some("none".to_string());

        update_memory(&store, &id, params).unwrap();

        let memory = store.get(&id).unwrap();
        assert!(memory.decay.is_some());
        let decay = memory.decay.unwrap();
        assert_eq!(decay.strategy, DecayStrategy::None);
    }
}
