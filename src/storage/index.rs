//! Index file operations for fast memory lookup

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use crate::types::{Memory, MemoryType, Status, ProvenanceSource};
use super::error::Result;
use std::path::Path;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub memories: Vec<IndexEntry>,
}

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

impl Default for Index {
    fn default() -> Self {
        Self {
            memories: Vec::new(),
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
