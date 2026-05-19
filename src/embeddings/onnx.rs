//! ONNX-based embedding provider using the fastembed crate.

use super::{EmbeddingError, EmbeddingProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::sync::{Arc, Mutex};

/// Specification for a fastembed-supported ONNX model.
#[derive(Debug, Clone)]
pub struct OnnxModelSpec {
    pub fastembed_model: EmbeddingModel,
    pub dimensions: usize,
    pub max_tokens: usize,
    /// Stable identifier for this model, persisted with embeddings to
    /// detect model swaps. Distinguishes fp32 vs int8. Used as
    /// `onnx/<name>` in the embedding fingerprint.
    pub name: &'static str,
}

/// all-MiniLM-L6-v2: 384-dimensional, 256 token context (fp32).
pub const ONNX_ALL_MINILM: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::AllMiniLML6V2,
    dimensions: 384,
    max_tokens: 256,
    name: "all-MiniLM-L6-v2",
};

/// all-MiniLM-L6-v2 int8-quantized (`Xenova/all-MiniLM-L6-v2`,
/// `onnx/model_quantized.onnx`, ~22 MB vs ~86 MB fp32). Same 384-dim
/// output; for the CPU latency/footprint A/B.
pub const ONNX_ALL_MINILM_Q: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::AllMiniLML6V2Q,
    dimensions: 384,
    max_tokens: 256,
    name: "all-MiniLM-L6-v2-q",
};

/// nomic-embed-text-v1.5: 768-dimensional, 8192 token context.
pub const ONNX_NOMIC_EMBED_TEXT: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::NomicEmbedTextV15,
    dimensions: 768,
    max_tokens: 8192,
    name: "nomic-embed-text-v1.5",
};

/// mxbai-embed-large-v1: 1024-dimensional, 512 token context.
pub const ONNX_MXBAI_EMBED_LARGE: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::MxbaiEmbedLargeV1,
    dimensions: 1024,
    name: "mxbai-embed-large-v1",
    max_tokens: 512,
};

/// Default embedding model (single source of truth, mirrors
/// `DEFAULT_T5_MODEL` / `DEFAULT_NLI_MODEL`).
///
/// int8 [`ONNX_ALL_MINILM_Q`] — the Lever B A/B showed it 1.4–1.9× faster
/// (2.5–6× under CPU contention), ~4× smaller, with no measurable
/// retrieval-quality loss (cosine vs fp32 ≈ 0.99, 4/4 ranking agreement).
/// Safe to default now that the embedding model identity is persisted and
/// a mismatch is surfaced/enforced via `embeddings.reindex_on_model_change`
/// (existing fp32 stores are flagged on connect and fixed by
/// `engramdb reindex --embeddings-only`).
pub const DEFAULT_ONNX_EMBEDDING: OnnxModelSpec = ONNX_ALL_MINILM_Q;

/// ONNX-based embedding provider using fastembed.
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
    model_id: String,
}

impl OnnxProvider {
    /// Create a new ONNX provider with the specified model, using the
    /// build-selected default execution backend.
    ///
    /// The model is cached in the platform cache directory so it only
    /// downloads once per machine.
    pub fn with_model(spec: OnnxModelSpec) -> Result<Self> {
        Self::with_model_on(spec, crate::onnx_ep::default_backend())
    }

    /// Create a new ONNX provider with the specified model on an explicit
    /// execution backend.
    ///
    /// Used by the benchmark suite to compare CPU vs Core ML on identical
    /// workloads; production code should use [`OnnxProvider::with_model`].
    pub fn with_model_on(spec: OnnxModelSpec, backend: crate::onnx_ep::Backend) -> Result<Self> {
        let cache_dir =
            crate::storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut options = InitOptions::new(spec.fastembed_model).with_cache_dir(cache_dir);
        let eps = crate::onnx_ep::providers_for(backend);
        if !eps.is_empty() {
            options = options.with_execution_providers(eps);
        }
        let model =
            TextEmbedding::try_new(options).context("Failed to initialize embedding model")?;

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dimensions: spec.dimensions,
            max_tokens: spec.max_tokens,
            model_id: format!("onnx/{}", spec.name),
        })
    }

    /// Create a new ONNX provider with [`DEFAULT_ONNX_EMBEDDING`].
    pub fn new() -> Result<Self> {
        Self::with_model(DEFAULT_ONNX_EMBEDDING)
    }

    /// Create [`DEFAULT_ONNX_EMBEDDING`] on an explicit backend.
    pub fn new_on(backend: crate::onnx_ep::Backend) -> Result<Self> {
        Self::with_model_on(DEFAULT_ONNX_EMBEDDING, backend)
    }

    /// Try to create a provider with the specified model, returning None if unavailable.
    pub fn try_with_model(spec: OnnxModelSpec) -> Option<Self> {
        Self::with_model(spec).ok()
    }

    /// Try to create the default model on an explicit backend, returning
    /// None if unavailable.
    pub fn try_new_on(backend: crate::onnx_ep::Backend) -> Option<Self> {
        Self::new_on(backend).ok()
    }

    /// Try to create a provider with the default model, returning None if unavailable.
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
        // Short-circuit empty input: fastembed's quantized models panic
        // ("chunk size must be non-zero") on an empty batch.
        if texts.is_empty() {
            return Ok(Vec::new());
        }
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

    fn model_id(&self) -> String {
        self.model_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    /// Shared embedding provider across all tests to avoid loading the ONNX
    /// model once per test (which causes OOM when parallel).
    static SHARED_PROVIDER: LazyLock<Option<OnnxProvider>> = LazyLock::new(OnnxProvider::try_new);

    fn try_provider() -> Option<&'static OnnxProvider> {
        let provider = SHARED_PROVIDER.as_ref();
        if provider.is_none() {
            eprintln!("Skipping: embedding model not available");
        }
        provider
    }

    #[test]
    fn test_provider_creation() {
        if let Some(provider) = try_provider() {
            assert_eq!(provider.dimensions(), 384);
        }
    }

    #[test]
    fn test_dimensions() {
        if let Some(provider) = try_provider() {
            assert_eq!(provider.dimensions(), 384);
        }
    }

    #[test]
    fn test_max_tokens() {
        if let Some(provider) = try_provider() {
            assert_eq!(provider.max_tokens(), 256);
        }
    }

    #[tokio::test]
    async fn test_embed_single() {
        if let Some(provider) = try_provider() {
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
        if let Some(provider) = try_provider() {
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
        if let Some(provider) = try_provider() {
            let result = provider.embed("").await;
            assert!(result.is_ok(), "Empty string embedding should succeed");

            let embedding = result.unwrap();
            assert_eq!(embedding.len(), 384);
        }
    }

    #[tokio::test]
    async fn test_embed_batch_empty_slice() {
        if let Some(provider) = try_provider() {
            let empty: Vec<&str> = vec![];
            let result = provider.embed_batch(&empty).await;
            assert!(result.is_ok(), "Empty batch should succeed");

            let embeddings = result.unwrap();
            assert!(embeddings.is_empty());
        }
    }

    #[tokio::test]
    async fn test_embed_consistency() {
        if let Some(provider) = try_provider() {
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
        if let Some(provider) = try_provider() {
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
