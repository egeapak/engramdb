//! ONNX-based embedding provider using the fastembed crate.

use super::{EmbeddingError, EmbeddingProvider};
use anyhow::{Context, Result};
use fastembed::{InitOptions, TextEmbedding};

/// ONNX-based embedding provider using all-MiniLM-L6-v2 model.
///
/// This provider uses the fastembed crate to generate embeddings locally
/// using ONNX Runtime. The model is downloaded and cached in a
/// platform-specific location so it is shared across all projects:
/// - macOS: `~/Library/Caches/engramdb/models`
/// - Linux: `$XDG_CACHE_HOME/engramdb/models` (default `~/.cache/engramdb/models`)
pub struct OnnxProvider {
    model: TextEmbedding,
    dimensions: usize,
}

impl OnnxProvider {
    /// Create a new ONNX provider with the all-MiniLM-L6-v2 model.
    ///
    /// The model is cached in the platform cache directory
    /// (`~/Library/Caches/engramdb/models` on macOS,
    /// `$XDG_CACHE_HOME/engramdb/models` on Linux) so it only downloads
    /// once per machine.
    ///
    /// # Returns
    /// A new provider instance, or an error if model initialization fails.
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .context("Could not determine cache directory")?
            .join("engramdb")
            .join("models");

        let options = InitOptions::default().with_cache_dir(cache_dir);
        let model =
            TextEmbedding::try_new(options).context("Failed to initialize embedding model")?;

        Ok(Self {
            model,
            dimensions: 384, // all-MiniLM-L6-v2 produces 384-dimensional embeddings
        })
    }

    /// Try to create a new provider, returning None if unavailable.
    ///
    /// This is useful for graceful degradation when embeddings are optional.
    ///
    /// # Returns
    /// Some(provider) if successful, None if model initialization fails.
    pub fn try_new() -> Option<Self> {
        Self::new().ok()
    }
}

impl EmbeddingProvider for OnnxProvider {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // fastembed's embed method takes a Vec of inputs and returns Vec<Vec<f32>>
        let embeddings = self
            .model
            .embed(vec![text.to_string()], None)
            .context("Failed to generate embedding")?;

        // Extract the first (and only) embedding
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Failed("No embedding returned".to_string()).into())
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Convert &str to String for fastembed
        let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();

        self.model
            .embed(texts, None)
            .context("Failed to generate batch embeddings")
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_creation() {
        // This test requires the model to be downloaded, so we use try_new
        let provider = OnnxProvider::try_new();
        assert!(provider.is_some(), "Provider should initialize");
    }

    #[test]
    fn test_dimensions() {
        if let Some(provider) = OnnxProvider::try_new() {
            assert_eq!(provider.dimensions(), 384);
        }
    }

    #[test]
    fn test_embed_single() {
        if let Some(provider) = OnnxProvider::try_new() {
            let result = provider.embed("Hello, world!");
            assert!(result.is_ok(), "Embedding should succeed");

            let embedding = result.unwrap();
            assert_eq!(embedding.len(), 384);

            // Embeddings should not be all zeros
            assert!(embedding.iter().any(|&x| x != 0.0));
        }
    }

    #[test]
    fn test_embed_batch() {
        if let Some(provider) = OnnxProvider::try_new() {
            let texts = vec!["First text", "Second text", "Third text"];
            let result = provider.embed_batch(&texts);
            assert!(result.is_ok(), "Batch embedding should succeed");

            let embeddings = result.unwrap();
            assert_eq!(embeddings.len(), 3);

            for embedding in embeddings {
                assert_eq!(embedding.len(), 384);
            }
        }
    }

    #[test]
    fn test_embed_empty_string() {
        if let Some(provider) = OnnxProvider::try_new() {
            let result = provider.embed("");
            assert!(result.is_ok(), "Empty string embedding should succeed");

            let embedding = result.unwrap();
            assert_eq!(embedding.len(), 384);
            // Should return a valid 384-dim vector (no panic/error)
        }
    }

    #[test]
    fn test_embed_batch_empty_slice() {
        if let Some(provider) = OnnxProvider::try_new() {
            let empty: Vec<&str> = vec![];
            let result = provider.embed_batch(&empty);
            assert!(result.is_ok(), "Empty batch should succeed");

            let embeddings = result.unwrap();
            assert!(embeddings.is_empty());
        }
    }

    #[test]
    fn test_embed_consistency() {
        if let Some(provider) = OnnxProvider::try_new() {
            let text = "hello";
            let embedding1 = provider.embed(text).unwrap();
            let embedding2 = provider.embed(text).unwrap();

            // Same text should produce identical embeddings
            assert_eq!(embedding1.len(), embedding2.len());
            for (a, b) in embedding1.iter().zip(embedding2.iter()) {
                assert!((a - b).abs() < 1e-6, "Embeddings should be identical");
            }
        }
    }

    #[test]
    fn test_embed_batch_single_matches_embed() {
        if let Some(provider) = OnnxProvider::try_new() {
            let text = "test text";
            let single_embedding = provider.embed(text).unwrap();
            let batch_embeddings = provider.embed_batch(&[text]).unwrap();

            assert_eq!(batch_embeddings.len(), 1);
            let batch_embedding = &batch_embeddings[0];

            // embed_batch(&["text"]) should equal vec![embed("text")]
            assert_eq!(single_embedding.len(), batch_embedding.len());
            for (a, b) in single_embedding.iter().zip(batch_embedding.iter()) {
                assert!(
                    (a - b).abs() < 1e-6,
                    "Single and batch embeddings should match"
                );
            }
        }
    }
}
