//! ONNX-based embedding provider using the fastembed crate.

use super::{EmbeddingError, EmbeddingProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{InitOptions, TextEmbedding};
use std::sync::{Arc, Mutex};

/// ONNX-based embedding provider using all-MiniLM-L6-v2 model.
///
/// This provider uses the fastembed crate to generate embeddings locally
/// using ONNX Runtime. The model is downloaded and cached in a
/// platform-specific location so it is shared across all projects:
/// - macOS: `~/Library/Caches/engramdb/models`
/// - Linux: `$XDG_CACHE_HOME/engramdb/models` (default `~/.cache/engramdb/models`)
pub struct OnnxProvider {
    model: Arc<Mutex<TextEmbedding>>,
    dimensions: usize,
    max_tokens: usize,
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
            model: Arc::new(Mutex::new(model)),
            dimensions: 384, // all-MiniLM-L6-v2 produces 384-dimensional embeddings
            max_tokens: 256, // all-MiniLM-L6-v2 truncates at 256 tokens
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

#[async_trait]
impl EmbeddingProvider for OnnxProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let text_owned = text.to_string();
        let model = Arc::clone(&self.model);
        // fastembed's embed method is CPU-bound, so run it in a blocking task
        let embeddings = tokio::task::spawn_blocking(move || {
            let mut model = model
                .lock()
                .map_err(|e| anyhow::anyhow!("Mutex poisoned: {}", e))?;
            model
                .embed(vec![text_owned], None)
                .context("Failed to generate embedding")
        })
        .await
        .context("Task panicked")??;

        // Extract the first (and only) embedding
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Failed("No embedding returned".to_string()).into())
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Convert &str to String for fastembed
        let texts_owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let model = Arc::clone(&self.model);

        tokio::task::spawn_blocking(move || {
            let mut model = model
                .lock()
                .map_err(|e| anyhow::anyhow!("Mutex poisoned: {}", e))?;
            model
                .embed(texts_owned, None)
                .context("Failed to generate batch embeddings")
        })
        .await
        .context("Task panicked")?
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn max_tokens(&self) -> usize {
        self.max_tokens
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
    fn test_max_tokens() {
        if let Some(provider) = OnnxProvider::try_new() {
            assert_eq!(provider.max_tokens(), 256);
        }
    }

    #[tokio::test]
    async fn test_embed_single() {
        if let Some(provider) = OnnxProvider::try_new() {
            let result = provider.embed("Hello, world!").await;
            assert!(result.is_ok(), "Embedding should succeed");

            let embedding = result.unwrap();
            assert_eq!(embedding.len(), 384);

            // Embeddings should not be all zeros
            assert!(embedding.iter().any(|&x| x != 0.0));
        }
    }

    #[tokio::test]
    async fn test_embed_batch() {
        if let Some(provider) = OnnxProvider::try_new() {
            let texts = vec!["First text", "Second text", "Third text"];
            let result = provider.embed_batch(&texts).await;
            assert!(result.is_ok(), "Batch embedding should succeed");

            let embeddings = result.unwrap();
            assert_eq!(embeddings.len(), 3);

            for embedding in embeddings {
                assert_eq!(embedding.len(), 384);
            }
        }
    }

    #[tokio::test]
    async fn test_embed_empty_string() {
        if let Some(provider) = OnnxProvider::try_new() {
            let result = provider.embed("").await;
            assert!(result.is_ok(), "Empty string embedding should succeed");

            let embedding = result.unwrap();
            assert_eq!(embedding.len(), 384);
            // Should return a valid 384-dim vector (no panic/error)
        }
    }

    #[tokio::test]
    async fn test_embed_batch_empty_slice() {
        if let Some(provider) = OnnxProvider::try_new() {
            let empty: Vec<&str> = vec![];
            let result = provider.embed_batch(&empty).await;
            assert!(result.is_ok(), "Empty batch should succeed");

            let embeddings = result.unwrap();
            assert!(embeddings.is_empty());
        }
    }

    #[tokio::test]
    async fn test_embed_consistency() {
        if let Some(provider) = OnnxProvider::try_new() {
            let text = "hello";
            let embedding1 = provider.embed(text).await.unwrap();
            let embedding2 = provider.embed(text).await.unwrap();

            // Same text should produce identical embeddings
            assert_eq!(embedding1.len(), embedding2.len());
            for (a, b) in embedding1.iter().zip(embedding2.iter()) {
                assert!((a - b).abs() < 1e-6, "Embeddings should be identical");
            }
        }
    }

    #[tokio::test]
    async fn test_embed_batch_single_matches_embed() {
        if let Some(provider) = OnnxProvider::try_new() {
            let text = "test text";
            let single_embedding = provider.embed(text).await.unwrap();
            let batch_embeddings = provider.embed_batch(&[text]).await.unwrap();

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
