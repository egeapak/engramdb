//! Memory compression operations.
//!
//! Provides two functions:
//! - `compress_candidates` — lists memories eligible for compression
//! - `compress_apply` — creates a summary memory that supersedes the given sources

use crate::ops::{create_memory, CreateParams};
use crate::storage::MemoryStore;
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
    pub superseded_count: usize,
}

/// List memories eligible for compression based on criticality threshold and scope.
pub fn compress_candidates(
    store: &MemoryStore,
    scope: Option<&str>,
    threshold: Option<f64>,
) -> Result<CompressCandidatesResult> {
    let entries = store.list()?;
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
pub fn compress_apply(
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

    // Validate all source IDs exist
    for id in &source_ids {
        store
            .get(id)
            .map_err(|_| anyhow::anyhow!("Source memory not found: {}", id))?;
    }

    let superseded_count = source_ids.len();

    let result = create_memory(
        store,
        CreateParams {
            type_: MemoryType::Context,
            content,
            summary: Some(summary),
            physical: vec!["/".to_string()],
            logical: scope.unwrap_or_default(),
            tags: tags.unwrap_or_default(),
            criticality: 0.5,
            confidence: 0.8,
            details: None,
            visibility: Visibility::Shared,
            provenance: Provenance::agent("compress"),
            supersedes: source_ids,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
        },
    )?;

    Ok(CompressApplyResult {
        new_id: result.id,
        superseded_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemoryType, Provenance, ProvenanceSource, Visibility};
    use tempfile::TempDir;

    fn setup_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();
        (temp_dir, store)
    }

    fn add_memory(
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
                summary: Some(summary.to_string()),
                physical: vec!["/".to_string()],
                logical,
                tags: vec![],
                criticality,
                confidence: 0.8,
                details: None,
                visibility: Visibility::Shared,
                provenance: Provenance::human(),
                supersedes: vec![],
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
            },
        )
        .unwrap();
        result.id
    }

    #[test]
    fn test_compress_candidates_basic() {
        let (_temp, store) = setup_store();

        add_memory(&store, MemoryType::Debug, "low crit debug", 0.1, vec![]);
        add_memory(
            &store,
            MemoryType::Decision,
            "high crit decision",
            0.9,
            vec![],
        );
        add_memory(
            &store,
            MemoryType::Context,
            "medium crit context",
            0.3,
            vec![],
        );

        let result = compress_candidates(&store, None, Some(0.4)).unwrap();

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

    #[test]
    fn test_compress_candidates_scope_filter() {
        let (_temp, store) = setup_store();

        add_memory(
            &store,
            MemoryType::Debug,
            "auth debug",
            0.1,
            vec!["auth".to_string()],
        );
        add_memory(
            &store,
            MemoryType::Debug,
            "db debug",
            0.1,
            vec!["db".to_string()],
        );

        let result = compress_candidates(&store, Some("auth"), Some(0.4)).unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.candidates[0].summary, "auth debug");
    }

    #[test]
    fn test_compress_candidates_empty() {
        let (_temp, store) = setup_store();

        add_memory(&store, MemoryType::Decision, "important", 0.9, vec![]);

        let result = compress_candidates(&store, None, Some(0.4)).unwrap();
        assert_eq!(result.total, 0);
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn test_compress_apply_basic() {
        let (_temp, store) = setup_store();

        let id1 = add_memory(&store, MemoryType::Debug, "debug 1", 0.1, vec![]);
        let id2 = add_memory(&store, MemoryType::Debug, "debug 2", 0.2, vec![]);

        let result = compress_apply(
            &store,
            vec![id1.clone(), id2.clone()],
            "Combined debug summary".to_string(),
            "Merged content from debug 1 and 2".to_string(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.superseded_count, 2);

        // Verify the new memory exists and has correct supersedes
        let new_memory = store.get(&result.new_id).unwrap();
        assert_eq!(new_memory.type_, MemoryType::Context);
        assert_eq!(new_memory.summary, "Combined debug summary");
        assert!(new_memory.supersedes.contains(&id1));
        assert!(new_memory.supersedes.contains(&id2));
    }

    #[test]
    fn test_compress_apply_invalid_source() {
        let (_temp, store) = setup_store();

        let result = compress_apply(
            &store,
            vec!["nonexistent-id".to_string()],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Source memory not found"));
    }

    #[test]
    fn test_compress_candidates_default_threshold_is_0_4() {
        let (_temp, store) = setup_store();

        add_memory(&store, MemoryType::Debug, "low crit", 0.3, vec![]);
        add_memory(&store, MemoryType::Decision, "high crit", 0.9, vec![]);

        let result = compress_candidates(&store, None, None).unwrap();

        assert_eq!(result.threshold, 0.4);
        assert_eq!(result.total, 1);
    }

    #[test]
    fn test_compress_candidates_includes_equal_to_threshold() {
        let (_temp, store) = setup_store();

        add_memory(&store, MemoryType::Debug, "below threshold", 0.39, vec![]);
        add_memory(&store, MemoryType::Debug, "at threshold", 0.4, vec![]);
        add_memory(&store, MemoryType::Debug, "above threshold", 0.41, vec![]);

        let result = compress_candidates(&store, None, Some(0.4)).unwrap();

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

    #[test]
    fn test_compress_candidates_physical_scope_match() {
        let (_temp, store) = setup_store();

        // Create memory with specific physical scope using Memory::new directly
        let mut mem_auth = Memory::new(
            MemoryType::Debug,
            "auth debug",
            "Auth content",
            Provenance::human(),
        );
        mem_auth.physical = vec!["/src/auth/".to_string()];
        mem_auth.criticality = 0.1;
        store.create(&mem_auth).unwrap();

        let mut mem_db = Memory::new(
            MemoryType::Debug,
            "db debug",
            "DB content",
            Provenance::human(),
        );
        mem_db.physical = vec!["/src/db/".to_string()];
        mem_db.criticality = 0.1;
        store.create(&mem_db).unwrap();

        let result = compress_candidates(&store, Some("/src/auth/"), Some(0.4)).unwrap();

        assert_eq!(result.total, 1);
        assert_eq!(result.candidates[0].summary, "auth debug");
    }

    #[test]
    fn test_compress_apply_empty_source_ids_returns_error() {
        let (_temp, store) = setup_store();

        let result = compress_apply(
            &store,
            vec![],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
    }

    #[test]
    fn test_compress_apply_creates_context_type_with_agent_provenance() {
        let (_temp, store) = setup_store();

        let id = add_memory(&store, MemoryType::Debug, "source debug", 0.1, vec![]);

        let result = compress_apply(
            &store,
            vec![id],
            "Compressed summary".to_string(),
            "Compressed content".to_string(),
            None,
            None,
        )
        .unwrap();

        let new_memory = store.get(&result.new_id).unwrap();
        assert_eq!(new_memory.type_, MemoryType::Context);
        assert_eq!(new_memory.provenance.source, ProvenanceSource::Agent);
        assert_eq!(new_memory.provenance.agent_id, Some("compress".to_string()));
    }

    #[test]
    fn test_compress_apply_with_scope_and_tags() {
        let (_temp, store) = setup_store();

        let id = add_memory(&store, MemoryType::Debug, "source", 0.1, vec![]);

        let result = compress_apply(
            &store,
            vec![id],
            "Scoped summary".to_string(),
            "Scoped content".to_string(),
            Some(vec!["app.auth".to_string(), "app.core".to_string()]),
            Some(vec!["compressed".to_string(), "auth".to_string()]),
        )
        .unwrap();

        let new_memory = store.get(&result.new_id).unwrap();
        assert_eq!(
            new_memory.logical,
            vec!["app.auth".to_string(), "app.core".to_string()]
        );
        assert_eq!(
            new_memory.tags,
            vec!["compressed".to_string(), "auth".to_string()]
        );
    }

    #[test]
    fn test_compress_apply_partial_invalid_source_ids_returns_error() {
        let (_temp, store) = setup_store();

        let valid_id = add_memory(&store, MemoryType::Debug, "valid source", 0.1, vec![]);
        let count_before = store.list().unwrap().len();

        let result = compress_apply(
            &store,
            vec![valid_id, "nonexistent-id".to_string()],
            "Summary".to_string(),
            "Content".to_string(),
            None,
            None,
        );

        assert!(result.is_err());
        // Verify no new memory was created
        let count_after = store.list().unwrap().len();
        assert_eq!(count_before, count_after);
    }
}
