//! Index file operations for fast memory lookup.
//!
//! This module provides the [`Index`] and [`IndexEntry`] types for efficient
//! memory queries without parsing all memory files. The index stores a subset
//! of memory metadata (id, type, summary, scopes, scores, timestamps) in JSON.
//!
//! Index operations include:
//! - Load/save index from/to index.json
//! - Add/update/remove entries
//! - Rebuild index from memory files on disk
//!
//! Separate indexes exist for shared and personal memories, both using the
//! same IndexEntry format. The index is automatically updated on create,
//! update, and delete operations.

use super::error::Result;
use crate::types::{Memory, MemoryType, ProvenanceSource, Status};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// In-memory representation of the index.json file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    /// All indexed memory entries
    pub memories: Vec<IndexEntry>,
}

/// Lightweight metadata entry for a single memory in the index.
///
/// Contains a subset of Memory fields sufficient for filtering and sorting
/// without loading full memory files from disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: MemoryType,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub physical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub criticality: f64,
    pub confidence: f64,
    pub provenance_source: ProvenanceSource,
    pub status: Status,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<&Memory> for IndexEntry {
    fn from(memory: &Memory) -> Self {
        Self {
            id: memory.id.clone(),
            type_: memory.type_,
            summary: memory.summary.clone(),
            physical: memory.physical.clone(),
            logical: memory.logical.clone(),
            tags: memory.tags.clone(),
            criticality: memory.criticality,
            confidence: memory.confidence,
            provenance_source: memory.provenance.source,
            status: memory.status,
            created_at: memory.created_at,
            updated_at: memory.updated_at,
            expires_at: memory.expires_at,
        }
    }
}

/// Load index from index.json
pub fn load_index(path: &Path) -> Result<Index> {
    if !path.exists() {
        return Ok(Index::default());
    }

    let content = fs::read_to_string(path)?;
    let index: Index = serde_json::from_str(&content)?;
    Ok(index)
}

/// Save index to index.json
pub fn save_index(path: &Path, index: &Index) -> Result<()> {
    let content = serde_json::to_string_pretty(index)?;
    fs::write(path, content)?;
    Ok(())
}

/// Add an entry to the index
pub fn add_entry(index: &mut Index, entry: IndexEntry) {
    // Remove existing entry with same ID if present
    index.memories.retain(|e| e.id != entry.id);
    index.memories.push(entry);
}

/// Remove an entry from the index by ID
pub fn remove_entry(index: &mut Index, id: &str) {
    index.memories.retain(|e| e.id != id);
}

/// Update an entry in the index
pub fn update_entry(index: &mut Index, entry: IndexEntry) {
    if let Some(existing) = index.memories.iter_mut().find(|e| e.id == entry.id) {
        *existing = entry;
    }
}

/// Rebuild index from memory files in a directory
pub fn rebuild_index_from_files(memories_dir: &Path) -> Result<Index> {
    let mut index = Index::default();

    if !memories_dir.exists() {
        return Ok(index);
    }

    for entry in fs::read_dir(memories_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            let content = fs::read_to_string(&path)?;
            let memory = super::memory_file::parse_memory_file(&content)?;
            let index_entry = IndexEntry::from(&memory);
            add_entry(&mut index, index_entry);
        }
    }

    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemoryType, Provenance, ProvenanceSource, Status, Visibility};
    use tempfile::NamedTempFile;

    fn create_test_memory(id: &str) -> Memory {
        Memory {
            id: id.to_string(),
            type_: MemoryType::Decision,
            summary: "Test summary".to_string(),
            content: "Test content".to_string(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec!["test.module".to_string()],
            tags: vec!["test".to_string()],
            criticality: 0.7,
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

    #[test]
    fn test_index_default_empty() {
        let index = Index::default();
        assert!(index.memories.is_empty());
    }

    #[test]
    fn test_add_entry_deduplicates() {
        let mut index = Index::default();

        let memory = create_test_memory("test-123");
        let entry1 = IndexEntry::from(&memory);
        add_entry(&mut index, entry1);

        assert_eq!(index.memories.len(), 1);
        assert_eq!(index.memories[0].summary, "Test summary");

        // Add same ID with different summary
        let mut memory2 = create_test_memory("test-123");
        memory2.summary = "Updated summary".to_string();
        let entry2 = IndexEntry::from(&memory2);
        add_entry(&mut index, entry2);

        // Should still have 1 entry with updated summary
        assert_eq!(index.memories.len(), 1);
        assert_eq!(index.memories[0].summary, "Updated summary");
    }

    #[test]
    fn test_remove_entry() {
        let mut index = Index::default();

        let memory1 = create_test_memory("test-123");
        let memory2 = create_test_memory("test-456");

        add_entry(&mut index, IndexEntry::from(&memory1));
        add_entry(&mut index, IndexEntry::from(&memory2));

        assert_eq!(index.memories.len(), 2);

        remove_entry(&mut index, "test-123");
        assert_eq!(index.memories.len(), 1);
        assert_eq!(index.memories[0].id, "test-456");

        // Removing nonexistent is a no-op
        remove_entry(&mut index, "nonexistent");
        assert_eq!(index.memories.len(), 1);
    }

    #[test]
    fn test_update_entry() {
        let mut index = Index::default();

        let memory = create_test_memory("test-789");
        let entry = IndexEntry::from(&memory);
        add_entry(&mut index, entry);

        assert_eq!(index.memories[0].summary, "Test summary");

        // Update the entry
        let mut updated_memory = create_test_memory("test-789");
        updated_memory.summary = "Modified summary".to_string();
        let updated_entry = IndexEntry::from(&updated_memory);
        update_entry(&mut index, updated_entry);

        assert_eq!(index.memories.len(), 1);
        assert_eq!(index.memories[0].summary, "Modified summary");

        // Updating nonexistent is a no-op
        let new_memory = create_test_memory("nonexistent");
        update_entry(&mut index, IndexEntry::from(&new_memory));
        assert_eq!(index.memories.len(), 1); // Still only 1
    }

    #[test]
    fn test_index_entry_from_memory() {
        let memory = create_test_memory("test-convert");
        let entry = IndexEntry::from(&memory);

        assert_eq!(entry.id, "test-convert");
        assert_eq!(entry.type_, MemoryType::Decision);
        assert_eq!(entry.summary, "Test summary");
        assert_eq!(entry.physical, vec!["/".to_string()]);
        assert_eq!(entry.logical, vec!["test.module".to_string()]);
        assert_eq!(entry.tags, vec!["test".to_string()]);
        assert_eq!(entry.criticality, 0.7);
        assert_eq!(entry.confidence, 0.9);
        assert_eq!(entry.provenance_source, ProvenanceSource::Human);
        assert_eq!(entry.status, Status::Active);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        let mut index = Index::default();
        let memory1 = create_test_memory("roundtrip-1");
        let memory2 = create_test_memory("roundtrip-2");

        add_entry(&mut index, IndexEntry::from(&memory1));
        add_entry(&mut index, IndexEntry::from(&memory2));

        // Save
        save_index(path, &index).unwrap();

        // Load
        let loaded = load_index(path).unwrap();

        assert_eq!(loaded.memories.len(), 2);
        assert!(loaded.memories.iter().any(|e| e.id == "roundtrip-1"));
        assert!(loaded.memories.iter().any(|e| e.id == "roundtrip-2"));
    }

    #[test]
    fn test_load_nonexistent_returns_default() {
        let nonexistent_path = Path::new("/tmp/nonexistent-engramdb-index-test.json");

        let index = load_index(nonexistent_path).unwrap();
        assert!(index.memories.is_empty());
    }
}
