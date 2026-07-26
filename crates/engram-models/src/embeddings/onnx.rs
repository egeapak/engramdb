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
/// output; for the CPU latency/footprint A/B. The default up to the
/// L12 switch — still selectable as `[embeddings].provider = "all-minilm-l6"`
/// by anyone who wants the smaller/faster model and no reindex.
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

/// nomic-embed-text-v1.5 int8-quantized (same repo, `onnx/model_quantized.onnx`,
/// ~131 MB vs ~522 MB fp32). Same 768-dim output and 8192 token context.
/// Used by the embedding-quality benchmark matrix (`examples/embed_matrix.rs`).
pub const ONNX_NOMIC_EMBED_TEXT_Q: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::NomicEmbedTextV15Q,
    dimensions: 768,
    max_tokens: 8192,
    name: "nomic-embed-text-v1.5-q",
};

/// bge-small-en-v1.5 int8-quantized (`Qdrant/bge-small-en-v1.5-onnx-Q`,
/// ~64 MB): retrieval-tuned English model, same 384 dims as MiniLM but a
/// 512-token context. Candidate replacement for the default; benchmarked in
/// `examples/embed_matrix.rs`.
pub const ONNX_BGE_SMALL_EN_Q: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::BGESmallENV15Q,
    dimensions: 384,
    max_tokens: 512,
    name: "bge-small-en-v1.5-q",
};

/// snowflake-arctic-embed-xs int8-quantized (`snowflake/snowflake-arctic-embed-xs`,
/// `onnx/model_quantized.onnx`, ~23 MB): retrieval-tuned 22M-parameter model in
/// the *same* size class as MiniLM-L6, same 384 dims, 512-token context.
/// Expects the `ARCTIC_QUERY_PREFIX` on the query side. Benchmarked in
/// `examples/embed_matrix.rs`.
pub const ONNX_ARCTIC_XS_Q: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::SnowflakeArcticEmbedXSQ,
    dimensions: 384,
    max_tokens: 512,
    name: "snowflake-arctic-embed-xs-q",
};

/// snowflake-arctic-embed-s int8-quantized (`snowflake/snowflake-arctic-embed-s`,
/// `onnx/model_quantized.onnx`, ~34 MB): the 33M-parameter step up from
/// [`ONNX_ARCTIC_XS_Q`], still 384 dims. Benchmarked in
/// `examples/embed_matrix.rs`.
pub const ONNX_ARCTIC_S_Q: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::SnowflakeArcticEmbedSQ,
    dimensions: 384,
    max_tokens: 512,
    name: "snowflake-arctic-embed-s-q",
};

/// all-MiniLM-L12-v2 int8-quantized (`Xenova/all-MiniLM-L12-v2`,
/// `onnx/model_quantized.onnx`, ~33 MB): the 12-layer sibling of
/// [`ONNX_ALL_MINILM_Q`], same 384 dims and 256-token context. **This is
/// [`DEFAULT_ONNX_EMBEDDING`]** — see there for the evidence.
pub const ONNX_ALL_MINILM_L12_Q: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::AllMiniLML12V2Q,
    dimensions: 384,
    max_tokens: 256,
    name: "all-MiniLM-L12-v2-q",
};

/// all-MiniLM-L12-v2 **fp32** (`Xenova/all-MiniLM-L12-v2`, `onnx/model.onnx`,
/// ~128 MB): the unquantized counterpart of [`ONNX_ALL_MINILM_L12_Q`], and the
/// same file [`crate::embeddings::TRACT_ALL_MINILM_L12`] loads — which is what
/// makes the tract-vs-ONNX numerical-equivalence test a like-for-like check.
pub const ONNX_ALL_MINILM_L12: OnnxModelSpec = OnnxModelSpec {
    fastembed_model: EmbeddingModel::AllMiniLML12V2,
    dimensions: 384,
    max_tokens: 256,
    name: "all-MiniLM-L12-v2",
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
/// int8 [`ONNX_ALL_MINILM_L12_Q`] — the model sweep in
/// `docs/contributors/embedding-model-alternatives.md` (R1/R2) measured it as
/// the best model on the retrieval corpus: MRR@10 0.958 vs 0.920 and P@1 0.938
/// vs 0.875 against the previous 6-layer default, beating even fp32 L6 (0.930)
/// — so the gain is depth, not quantization precision — and returning
/// identical rankings on 3/3 repeats where the L6 int8 model swings ±0.023 MRR
/// run-to-run on identical input. Costs 2.0× warm embed latency (2.83 → 5.70 ms)
/// and +10 MB on disk; cold start actually improves (171 → 146 ms).
///
/// Same family, same 384 dims, same 256-token window, same tokenizer, so this
/// is a drop-in: no config or schema change. Existing stores detect the
/// `model_id()` change through the manifest fingerprint and are repaired by
/// `engramdb reindex --embeddings-only`, exactly as the fp32→int8 switch was.
/// Pin the old model with `[embeddings].provider = "all-minilm-l6"`.
pub const DEFAULT_ONNX_EMBEDDING: OnnxModelSpec = ONNX_ALL_MINILM_L12_Q;

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
        Self::with_model_on(spec, engram_onnx::default_backend())
    }

    /// Create a new ONNX provider with the specified model on an explicit
    /// execution backend.
    ///
    /// Used by the benchmark suite to compare CPU vs Core ML on identical
    /// workloads; production code should use [`OnnxProvider::with_model`].
    pub fn with_model_on(spec: OnnxModelSpec, backend: engram_onnx::Backend) -> Result<Self> {
        let cache_dir =
            engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

        // Offline mode: don't let fastembed reach the network. If the model
        // isn't already cached, fail fast rather than downloading it.
        if engram_storage::paths::offline_enabled() {
            let repo = TextEmbedding::get_model_info(&spec.fastembed_model)
                .map(|info| info.model_code.clone())
                .unwrap_or_default();
            if !engram_storage::paths::hf_repo_cached(&repo) {
                anyhow::bail!(
                    "offline mode (ENGRAMDB_OFFLINE) and model '{}' ({}) is not cached",
                    spec.name,
                    repo
                );
            }
        }

        // Propagate the spec's context window: fastembed defaults tokenizer
        // truncation to 512, so without this a long-context model (nomic's
        // 8192) silently drops everything past token 512 — while the chunker
        // budgets chunks against `max_tokens()` and trusts the full window.
        let mut options = InitOptions::new(spec.fastembed_model)
            .with_cache_dir(cache_dir)
            .with_max_length(spec.max_tokens);
        let eps = engram_onnx::providers_for(backend);
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
    pub fn new_on(backend: engram_onnx::Backend) -> Result<Self> {
        Self::with_model_on(DEFAULT_ONNX_EMBEDDING, backend)
    }

    /// Try to create a provider with the specified model, returning None if unavailable.
    pub fn try_with_model(spec: OnnxModelSpec) -> Option<Self> {
        Self::with_model(spec).ok()
    }

    /// Try to create the default model on an explicit backend, returning
    /// None if unavailable.
    pub fn try_new_on(backend: engram_onnx::Backend) -> Option<Self> {
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

    /// Agreement floor for "the same text embeds to the same vector" **on the
    /// int8 default**.
    ///
    /// These two assertions used to demand element-wise equality within 1e-6,
    /// and flaked. That bound was never sound: the int8 models are not
    /// reproducible on a busy machine. `examples/embed_determinism_probe.rs`
    /// embeds one text 30 times through one provider and reports, under CPU
    /// saturation, a minimum pairwise cosine of 0.97 (L12 default) and 0.57
    /// (the older L6 int8 model); the full test suite has produced 0.71. The
    /// fp32 model is bit-exact in every condition — see
    /// [`fp32_embedding_is_bit_exact`], which is where the real determinism
    /// guard now lives.
    ///
    /// So this floor is deliberately loose. It is not a quality bar; it is a
    /// wiring check that still fails loudly for what these tests exist to catch
    /// — a provider returning a zero or unrelated vector (cosine ≈ 0). Tighten
    /// it only if the underlying instability is fixed (e.g. by pinning ORT's
    /// intra-op thread count). See
    /// `docs/contributors/embedding-model-alternatives.md` (R6).
    const CONSISTENCY_MIN_COSINE: f32 = 0.4;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            return 0.0;
        }
        dot / (na * nb)
    }

    #[tokio::test]
    async fn test_embed_consistency() {
        if let Some(provider) = try_provider() {
            let text = "hello";
            let embedding1 = provider.embed(text).await.unwrap();
            let embedding2 = provider.embed(text).await.unwrap();

            assert_eq!(embedding1.len(), embedding2.len());
            let cos = cosine(&embedding1, &embedding2);
            assert!(
                cos >= CONSISTENCY_MIN_COSINE,
                "repeated embed() diverged: cosine {cos} < {CONSISTENCY_MIN_COSINE}"
            );
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

            assert_eq!(single_embedding.len(), batch_embedding.len());
            let cos = cosine(&single_embedding, batch_embedding);
            assert!(
                cos >= CONSISTENCY_MIN_COSINE,
                "embed() and embed_batch() diverged: cosine {cos} < {CONSISTENCY_MIN_COSINE}"
            );
        }
    }

    /// The determinism guard the int8 tests above can no longer be: fp32 is
    /// bit-exact across calls and across the single/batch paths, in every load
    /// condition measured. If this ever flakes, the provider — not the
    /// quantization — is broken.
    ///
    /// Self-skips when the ~86 MB fp32 model isn't cached (CI / offline).
    #[tokio::test]
    async fn fp32_embedding_is_bit_exact() {
        let Some(provider) = OnnxProvider::try_with_model(ONNX_ALL_MINILM) else {
            eprintln!("Skipping: fp32 ONNX model not available");
            return;
        };
        let text = "deterministic across calls and across the batch path";
        let a = provider.embed(text).await.unwrap();
        let b = provider.embed(text).await.unwrap();
        let c = provider.embed_batch(&[text]).await.unwrap().remove(0);
        assert_eq!(a, b, "repeated fp32 embed() must be bit-identical");
        assert_eq!(a, c, "fp32 embed() and embed_batch() must be bit-identical");
    }
}
