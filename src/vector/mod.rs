//! Vector storage for embedding-based semantic search
//!
//! This module provides an abstraction over vector databases for storing and searching
//! memory embeddings. The primary implementation uses LanceDB for embedded vector storage.

use anyhow::Result;

mod lancedb;

pub use lancedb::LanceDbStore;

/// Metadata associated with a vector in the store
#[derive(Debug, Clone)]
pub struct VectorMetadata {
    /// Memory type (e.g., "decision", "convention", "hazard")
    pub type_: String,
    /// Criticality score (0.0 to 1.0)
    pub criticality: f64,
    /// Physical scope paths (file globs)
    pub physical: Vec<String>,
    /// Logical scope domains (dot-notation)
    pub logical: Vec<String>,
    /// Searchable tags
    pub tags: Vec<String>,
}

/// A vector search result with ID and similarity score
#[derive(Debug, Clone)]
pub struct VectorMatch {
    /// Memory ID
    pub id: String,
    /// Cosine similarity score (higher is better)
    pub score: f64,
}

/// Trait for vector storage backends
///
/// Implementations store embeddings and support similarity search.
/// All operations are synchronous from the caller's perspective.
pub trait VectorStore: Send + Sync {
    /// Insert or update a vector with metadata
    ///
    /// # Arguments
    /// * `id` - Unique identifier for the memory
    /// * `vector` - Embedding vector (should match dimensions expected by the store)
    /// * `metadata` - Associated metadata for filtering and context
    fn upsert(&self, id: &str, vector: Vec<f32>, metadata: VectorMetadata) -> Result<()>;

    /// Search for similar vectors
    ///
    /// # Arguments
    /// * `query` - Query embedding vector
    /// * `limit` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of matches sorted by similarity score (descending)
    fn search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<VectorMatch>>;

    /// Delete a vector from the store
    ///
    /// # Arguments
    /// * `id` - Unique identifier of the vector to delete
    fn delete(&self, id: &str) -> Result<()>;
}
