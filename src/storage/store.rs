//! Main storage orchestrator - MemoryStore

use crate::types::{Memory, MemoryUpdate, Visibility};
use super::error::{Result, StorageError};
use super::{paths, project_id, manifest, index, memory_file};
use std::path::{Path, PathBuf};
use std::fs;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub project_id: String,
    pub project_path: String,
    pub last_opened: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub projects: Vec<RegistryEntry>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
        }
    }
}

pub struct MemoryStore {
    pub project_dir: PathBuf,
    pub project_id: String,
}

impl MemoryStore {
    /// Initialize a new EngramDB store in the given directory
    pub fn init(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        // Create directory structure
        fs::create_dir_all(&engramdb_dir)?;
        fs::create_dir_all(paths::memories_dir(dir))?;

        // Create manifest.toml
        let manifest_path = engramdb_dir.join("manifest.toml");
        let manifest = manifest::Manifest::default();
        manifest::save_manifest(&manifest_path, &manifest)?;

        // Create empty config.toml
        let config_path = engramdb_dir.join("config.toml");
        fs::write(config_path, "# EngramDB configuration\n# See documentation for available settings\n")?;

        // Create empty index.json
        let index_path = engramdb_dir.join("index.json");
        let empty_index = index::Index::default();
        index::save_index(&index_path, &empty_index)?;

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Create personal directories
        fs::create_dir_all(paths::personal_memories_dir(&project_id))?;
        fs::create_dir_all(paths::lancedb_dir(&project_id))?;

        // Update registry
        Self::update_registry(dir, &project_id)?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
        })
    }

    /// Open an existing EngramDB store
    pub fn open(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Update registry
        Self::update_registry(dir, &project_id)?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
        })
    }

    /// Create a new memory
    pub fn create(&self, memory: &Memory) -> Result<String> {
        let memories_dir = self.get_memories_dir(&memory.visibility);
        fs::create_dir_all(&memories_dir)?;

        // Write memory file
        let file_path = memories_dir.join(format!("{}.md", memory.id));
        let content = memory_file::write_memory_file(memory)?;
        fs::write(&file_path, content)?;

        // Update index
        let index_path = self.get_index_path(&memory.visibility);
        let mut idx = index::load_index(&index_path)?;
        let entry = index::IndexEntry::from(memory);
        index::add_entry(&mut idx, entry);
        index::save_index(&index_path, &idx)?;

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(memory.id.clone())
    }

    /// Get a memory by ID (supports prefix matching)
    pub fn get(&self, id: &str) -> Result<Memory> {
        // Try shared memories first
        if let Ok(memory) = self.get_from_dir(id, &paths::memories_dir(&self.project_dir)) {
            return Ok(memory);
        }

        // Try personal memories
        self.get_from_dir(id, &paths::personal_memories_dir(&self.project_id))
    }

    fn get_from_dir(&self, id: &str, dir: &Path) -> Result<Memory> {
        if !dir.exists() {
            return Err(StorageError::NotFound(id.to_string()));
        }

        let mut matches = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                if filename.starts_with(id) {
                    matches.push(path);
                }
            }
        }

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => {
                let content = fs::read_to_string(&matches[0])?;
                memory_file::parse_memory_file(&content)
            }
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// Update a memory
    pub fn update(&self, id: &str, updates: MemoryUpdate) -> Result<()> {
        // Get existing memory
        let mut memory = self.get(id)?;
        let old_visibility = memory.visibility;

        // Apply updates
        updates.apply_to(&mut memory);

        // Check if visibility changed
        if memory.visibility != old_visibility {
            // Delete from old location
            self.delete_from_dir(id, &self.get_memories_dir(&old_visibility))?;

            // Write to new location
            self.create(&memory)?;
        } else {
            // Write updated memory
            let memories_dir = self.get_memories_dir(&memory.visibility);
            let file_path = memories_dir.join(format!("{}.md", memory.id));
            let content = memory_file::write_memory_file(&memory)?;
            fs::write(&file_path, content)?;

            // Update index
            let index_path = self.get_index_path(&memory.visibility);
            let mut idx = index::load_index(&index_path)?;
            let entry = index::IndexEntry::from(&memory);
            index::update_entry(&mut idx, entry);
            index::save_index(&index_path, &idx)?;
        }

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(())
    }

    /// Delete a memory
    pub fn delete(&self, id: &str) -> Result<()> {
        // Try to delete from shared
        if self.delete_from_dir(id, &paths::memories_dir(&self.project_dir)).is_ok() {
            self.update_manifest_stats()?;
            return Ok(());
        }

        // Try to delete from personal
        self.delete_from_dir(id, &paths::personal_memories_dir(&self.project_id))?;
        self.update_manifest_stats()?;
        Ok(())
    }

    fn delete_from_dir(&self, id: &str, dir: &Path) -> Result<()> {
        if !dir.exists() {
            return Err(StorageError::NotFound(id.to_string()));
        }

        let mut matches = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                if filename.starts_with(id) {
                    let fname = filename.to_string();
                    matches.push((path, fname));
                }
            }
        }

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => {
                let (path, memory_id) = &matches[0];
                fs::remove_file(path)?;

                // Update index based on directory
                let visibility = if dir == paths::memories_dir(&self.project_dir) {
                    Visibility::Shared
                } else {
                    Visibility::Personal
                };

                let index_path = self.get_index_path(&visibility);
                let mut idx = index::load_index(&index_path)?;
                index::remove_entry(&mut idx, memory_id);
                index::save_index(&index_path, &idx)?;

                Ok(())
            }
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// List all memories (returns index entries)
    pub fn list(&self) -> Result<Vec<index::IndexEntry>> {
        let mut all_entries = Vec::new();

        // Load shared index
        let shared_index_path = paths::project_dir(&self.project_dir).join("index.json");
        if let Ok(idx) = index::load_index(&shared_index_path) {
            all_entries.extend(idx.memories);
        }

        // Load personal index
        let personal_index_path = paths::personal_dir(&self.project_id).join("index.json");
        if let Ok(idx) = index::load_index(&personal_index_path) {
            all_entries.extend(idx.memories);
        }

        Ok(all_entries)
    }

    /// Rebuild index from memory files
    pub fn reindex(&self) -> Result<usize> {
        let mut count = 0;

        // Reindex shared memories
        let shared_dir = paths::memories_dir(&self.project_dir);
        if shared_dir.exists() {
            let idx = index::rebuild_index_from_files(&shared_dir)?;
            count += idx.memories.len();
            let index_path = paths::project_dir(&self.project_dir).join("index.json");
            index::save_index(&index_path, &idx)?;
        }

        // Reindex personal memories
        let personal_dir = paths::personal_memories_dir(&self.project_id);
        if personal_dir.exists() {
            let idx = index::rebuild_index_from_files(&personal_dir)?;
            count += idx.memories.len();
            let index_path = paths::personal_dir(&self.project_id).join("index.json");
            index::save_index(&index_path, &idx)?;
        }

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(count)
    }

    // Helper methods

    fn get_memories_dir(&self, visibility: &Visibility) -> PathBuf {
        match visibility {
            Visibility::Shared => paths::memories_dir(&self.project_dir),
            Visibility::Personal => paths::personal_memories_dir(&self.project_id),
        }
    }

    fn get_index_path(&self, visibility: &Visibility) -> PathBuf {
        match visibility {
            Visibility::Shared => paths::project_dir(&self.project_dir).join("index.json"),
            Visibility::Personal => paths::personal_dir(&self.project_id).join("index.json"),
        }
    }

    fn update_manifest_stats(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path)?;

        // Count memories and collect logical scopes
        let entries = self.list()?;
        let memory_count = entries.len();
        let logical_scopes: Vec<String> = entries
            .iter()
            .flat_map(|e| e.logical.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        manifest::update_stats(&mut manifest, memory_count, logical_scopes);
        manifest::save_manifest(&manifest_path, &manifest)?;

        Ok(())
    }

    fn update_registry(dir: &Path, project_id: &str) -> Result<()> {
        let registry_path = paths::registry_path();

        // Create registry directory if needed
        if let Some(parent) = registry_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Load or create registry
        let mut registry = if registry_path.exists() {
            let content = fs::read_to_string(&registry_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Registry::default()
        };

        // Update or add entry
        let abs_path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let path_str = abs_path.to_string_lossy().to_string();

        if let Some(entry) = registry.projects.iter_mut().find(|e| e.project_id == project_id) {
            entry.last_opened = chrono::Utc::now();
            entry.project_path = path_str;
        } else {
            registry.projects.push(RegistryEntry {
                project_id: project_id.to_string(),
                project_path: path_str,
                last_opened: chrono::Utc::now(),
            });
        }

        // Save registry
        let content = serde_json::to_string_pretty(&registry)?;
        fs::write(&registry_path, content)?;

        Ok(())
    }
}
