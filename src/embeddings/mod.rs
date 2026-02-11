//! Embedding provider abstraction and implementations.
//!
//! This module provides a trait-based interface for text embedding generation,
//! along with an ONNX-based implementation using the fastembed crate.

mod onnx;

pub use onnx::OnnxProvider;

use anyhow::Result;
use async_trait::async_trait;

/// Error types for embedding operations.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    /// Embedding model is unavailable or failed to initialize.
    #[error("Embedding model unavailable: {0}")]
    Unavailable(String),

    /// Embedding generation failed.
    #[error("Embedding failed: {0}")]
    Failed(String),
}

/// Trait for text embedding providers.
///
/// Implementations should be thread-safe (Send + Sync) to allow
/// concurrent embedding generation.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding for a single text input.
    ///
    /// # Arguments
    /// * `text` - The text to embed
    ///
    /// # Returns
    /// A vector of floats representing the embedding, or an error.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Generate embeddings for multiple text inputs in a batch.
    ///
    /// Batch processing is typically more efficient than calling `embed`
    /// multiple times sequentially.
    ///
    /// # Arguments
    /// * `texts` - A slice of text strings to embed
    ///
    /// # Returns
    /// A vector of embeddings (one per input text), or an error.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Get the dimensionality of embeddings produced by this provider.
    ///
    /// # Returns
    /// The number of dimensions in each embedding vector.
    fn dimensions(&self) -> usize;
}
