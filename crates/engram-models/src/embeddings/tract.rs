//! Pure-Rust embedding provider using the `tract-onnx` engine.
//!
//! This is the fallback backend for platforms with **no prebuilt ONNX Runtime**
//! — notably Intel Mac (`x86_64-apple-darwin`), where `fastembed`'s pinned
//! `ort` (ONNX Runtime 1.24 / API-24) has no downloadable binary. `tract` is
//! Sonos's pure-Rust ONNX engine: no native library, builds anywhere Rust
//! builds, nothing to install at runtime.
//!
//! Trade-offs, measured in `experiments/tract-vs-ort/`:
//! - Output is **bit-identical** to ONNX Runtime on the fp32 model
//!   (cosine 1.00000).
//! - It is **~3× slower** single / ~6× batched. Acceptable for an on-demand
//!   memory store (embed on create/query, not a hot loop), and it *works*
//!   where ORT has no runtime at all.
//! - It **cannot load the int8-quantized MiniLM** (a static shape-analysis
//!   failure on the quantized export), so this provider always uses the
//!   **fp32** model (`Qdrant/all-MiniLM-L6-v2-onnx`, `model.onnx`).
//!
//! tract needs concrete input shapes, so the runnable is built once at load
//! for a fixed `[1, max_tokens]` shape and every request is padded/truncated
//! to that width — the mask zeroes the padding so pooling is unaffected. The
//! `RunnableModel` is `Arc`-wrapped and cheap to clone/share across sessions.

use super::{EmbeddingError, EmbeddingProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokenizers::Tokenizer;
use tract_onnx::prelude::*;

/// Specification for a tract-loaded fp32 ONNX embedding model.
///
/// Distinct from [`super::OnnxModelSpec`], which carries a
/// `fastembed::EmbeddingModel` (absent on a tract-only build). This spec
/// names the raw HuggingFace files tract loads directly via `hf-hub`.
#[derive(Debug, Clone, Copy)]
pub struct TractModelSpec {
    /// HuggingFace repo id (must host an **fp32** ONNX export).
    pub repo: &'static str,
    /// ONNX model filename within the repo.
    pub model_file: &'static str,
    /// Tokenizer filename within the repo.
    pub tokenizer_file: &'static str,
    pub dimensions: usize,
    pub max_tokens: usize,
    /// Stable identifier, persisted with embeddings as `tract/<name>` so the
    /// manifest fingerprint detects a backend swap (tract/fp32 vs onnx/int8)
    /// and drives a `reindex --embeddings-only`.
    pub name: &'static str,
}

/// all-MiniLM-L6-v2 fp32 (`Qdrant/all-MiniLM-L6-v2-onnx`, `model.onnx`),
/// 384-dimensional, 256 token context. The int8 default does not load under
/// tract, so the tract backend uses this fp32 export.
pub const TRACT_ALL_MINILM: TractModelSpec = TractModelSpec {
    repo: "Qdrant/all-MiniLM-L6-v2-onnx",
    model_file: "model.onnx",
    tokenizer_file: "tokenizer.json",
    dimensions: 384,
    max_tokens: 256,
    name: "all-MiniLM-L6-v2-fp32",
};

/// The default tract embedding model.
pub const DEFAULT_TRACT_EMBEDDING: TractModelSpec = TRACT_ALL_MINILM;

type RunnableTractModel = Arc<RunnableModel<TypedFact, Box<dyn TypedOp>>>;

/// Which logical role a graph input plays, discovered by name so models that
/// export inputs in a different order (or omit `token_type_ids`) still work.
#[derive(Clone, Copy, Debug)]
enum InputRole {
    Ids,
    Mask,
    Types,
}

fn classify_input(name: &str) -> InputRole {
    let n = name.to_lowercase();
    if n.contains("token_type") || n.contains("type_ids") {
        InputRole::Types
    } else if n.contains("attention") || n.contains("mask") {
        InputRole::Mask
    } else {
        InputRole::Ids
    }
}

/// Pure-Rust embedding provider backed by tract.
pub struct TractEmbeddingProvider {
    model: RunnableTractModel,
    input_roles: Vec<InputRole>,
    tokenizer: Arc<Tokenizer>,
    dimensions: usize,
    max_tokens: usize,
    model_id: String,
}

impl TractEmbeddingProvider {
    /// Load [`DEFAULT_TRACT_EMBEDDING`].
    pub fn new() -> Result<Self> {
        Self::with_model(DEFAULT_TRACT_EMBEDDING)
    }

    /// Load a specific tract model spec, building a runnable for the fixed
    /// `[1, max_tokens]` shape. The model + tokenizer are fetched via `hf-hub`
    /// into the shared model cache (same path/offline behavior as every other
    /// EngramDB model) — no `fastembed`.
    pub fn with_model(spec: TractModelSpec) -> Result<Self> {
        let cache_dir =
            engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

        // Offline mode: refuse to download an uncached model, failing fast.
        if engram_storage::paths::offline_enabled()
            && !engram_storage::paths::hf_repo_cached(spec.repo)
        {
            anyhow::bail!(
                "offline mode (ENGRAMDB_OFFLINE) and tract model '{}' ({}) is not cached",
                spec.name,
                spec.repo
            );
        }

        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir)
            .build()
            .context("Failed to initialize HuggingFace API")?;
        let repo = api.model(spec.repo.to_string());
        let model_path = repo
            .get(spec.model_file)
            .with_context(|| format!("Failed to fetch tract model {}", spec.model_file))?;
        let tokenizer_path = repo
            .get(spec.tokenizer_file)
            .with_context(|| format!("Failed to fetch tokenizer {}", spec.tokenizer_file))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {e}"))?;

        let mut infer = tract_onnx::onnx()
            .model_for_path(&model_path)
            .with_context(|| format!("tract model_for_path {}", model_path.display()))?;

        // Discover input names/order from the inference graph, then set a
        // concrete i64 [1, max_tokens] fact for each (tract needs concrete
        // shapes; a fixed width lets us build the runnable exactly once).
        let input_names: Vec<String> = infer
            .input_outlets()?
            .iter()
            .map(|o| infer.node(o.node).name.clone())
            .collect();
        let input_roles: Vec<InputRole> = input_names.iter().map(|n| classify_input(n)).collect();

        for i in 0..input_names.len() {
            infer = infer.with_input_fact(
                i,
                InferenceFact::dt_shape(i64::datum_type(), tvec!(1, spec.max_tokens)),
            )?;
        }

        // Prefer the optimized graph; fall back to typed-unoptimized rather
        // than failing outright (fp32 MiniLM optimizes cleanly).
        let model = match infer.clone().into_optimized() {
            Ok(opt) => opt.into_runnable()?,
            Err(e) => {
                tracing::warn!("tract into_optimized() failed, using unoptimized graph: {e}");
                infer.into_typed()?.into_runnable()?
            }
        };

        Ok(Self {
            // `into_runnable()` already yields an `Arc`-shared plan.
            model,
            input_roles,
            tokenizer: Arc::new(tokenizer),
            dimensions: spec.dimensions,
            max_tokens: spec.max_tokens,
            model_id: format!("tract/{}", spec.name),
        })
    }

    /// Try to load a provider with the given spec, returning `None` on failure.
    pub fn try_with_model(spec: TractModelSpec) -> Option<Self> {
        Self::with_model(spec).ok()
    }

    /// Try to load the default provider, returning `None` on failure.
    pub fn try_new() -> Option<Self> {
        Self::new().ok()
    }

    /// Tokenize `text`, run the fixed-shape graph, and mean-pool + L2-normalize
    /// to a single embedding. Runs on the calling thread (the async wrappers
    /// offload this to `spawn_blocking`).
    fn embed_blocking(&self, text: &str) -> Result<Vec<f32>> {
        let enc = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
        let seq = self.max_tokens;

        // Pad/truncate to the fixed [1, seq] shape. Padding tokens carry
        // mask=0 so they contribute nothing to the pooled mean.
        let mut ids = vec![0i64; seq];
        let mut mask = vec![0i64; seq];
        let types = vec![0i64; seq];
        let eids = enc.get_ids();
        let emask = enc.get_attention_mask();
        let n = eids.len().min(seq);
        for t in 0..n {
            ids[t] = eids[t] as i64;
            mask[t] = emask[t] as i64;
        }

        let make = |role: InputRole| -> TValue {
            let flat = match role {
                InputRole::Ids => ids.clone(),
                InputRole::Mask => mask.clone(),
                InputRole::Types => types.clone(),
            };
            let t: Tensor = tract_ndarray::Array2::<i64>::from_shape_vec((1, seq), flat)
                .expect("fixed [1, seq] shape")
                .into();
            t.into()
        };
        let inputs: TVec<TValue> = self.input_roles.iter().map(|r| make(*r)).collect();
        let result = self.model.run(inputs)?;
        let view = result[0].to_plain_array_view::<f32>()?;
        let hidden: Vec<f32> = view.iter().copied().collect();

        Ok(pool_and_normalize(&hidden, seq, self.dimensions, &mask))
    }
}

/// Masked mean-pool over the sequence, then L2-normalize. `hidden` is the flat
/// `[1, seq, dim]` last-hidden-state; returns one `[dim]` embedding. Matches
/// sentence-transformers all-MiniLM pooling (lifted from the experiment).
fn pool_and_normalize(hidden: &[f32], seq: usize, dim: usize, mask: &[i64]) -> Vec<f32> {
    let mut acc = vec![0f32; dim];
    let mut denom = 0f32;
    for (t, &mask_t) in mask.iter().take(seq).enumerate() {
        let m = mask_t as f32;
        if m == 0.0 {
            continue;
        }
        denom += m;
        let base = t * dim;
        for (a, &h) in acc.iter_mut().zip(&hidden[base..base + dim]) {
            *a += h * m;
        }
    }
    let denom = denom.max(1e-9);
    for v in acc.iter_mut() {
        *v /= denom;
    }
    let norm: f32 = acc.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for v in acc.iter_mut() {
        *v /= norm;
    }
    acc
}

#[async_trait]
impl EmbeddingProvider for TractEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let text = text.to_string();
        let model = self.clone_handle();
        tokio::task::spawn_blocking(move || model.embed_blocking(&text))
            .await
            .context("Task panicked")?
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let model = self.clone_handle();
        tokio::task::spawn_blocking(move || {
            texts
                .iter()
                .map(|t| model.embed_blocking(t))
                .collect::<Result<Vec<_>>>()
        })
        .await
        .context("Task panicked")?
        .map_err(|e: anyhow::Error| {
            EmbeddingError::Failed(format!("tract batch embed failed: {e}")).into()
        })
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

impl TractEmbeddingProvider {
    /// A cheap clone sharing the `Arc`-wrapped runnable + tokenizer, for moving
    /// into a `spawn_blocking` closure.
    fn clone_handle(&self) -> Self {
        Self {
            model: Arc::clone(&self.model),
            input_roles: self.input_roles.clone(),
            tokenizer: Arc::clone(&self.tokenizer),
            dimensions: self.dimensions,
            max_tokens: self.max_tokens,
            model_id: self.model_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    static SHARED: LazyLock<Option<TractEmbeddingProvider>> =
        LazyLock::new(TractEmbeddingProvider::try_new);

    fn provider() -> Option<&'static TractEmbeddingProvider> {
        let p = SHARED.as_ref();
        if p.is_none() {
            eprintln!("Skipping: tract fp32 model not available");
        }
        p
    }

    #[test]
    fn model_id_is_distinct_fp32() {
        // Pure spec check — no model load, always runs.
        assert_eq!(
            format!("tract/{}", TRACT_ALL_MINILM.name),
            "tract/all-MiniLM-L6-v2-fp32"
        );
        assert_eq!(TRACT_ALL_MINILM.dimensions, 384);
    }

    #[tokio::test]
    async fn embed_has_right_shape_and_is_normalized() {
        if let Some(p) = provider() {
            let v = p
                .embed("EngramDB is a memory store for coding agents.")
                .await;
            let v = v.expect("embed ok");
            assert_eq!(v.len(), 384);
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "L2-normalized, got {norm}");
        }
    }

    #[tokio::test]
    async fn batch_matches_single() {
        if let Some(p) = provider() {
            let text = "vector databases enable semantic search";
            let single = p.embed(text).await.unwrap();
            let batch = p.embed_batch(&[text]).await.unwrap();
            assert_eq!(batch.len(), 1);
            for (a, b) in single.iter().zip(&batch[0]) {
                assert!((a - b).abs() < 1e-6);
            }
        }
    }

    #[tokio::test]
    async fn empty_batch_is_empty() {
        if let Some(p) = provider() {
            let out = p.embed_batch(&[]).await.unwrap();
            assert!(out.is_empty());
        }
    }

    /// Correctness bar for the backend swap: tract's fp32 output must match the
    /// fp32 ONNX Runtime output (the experiment measured cosine 1.00000). Runs
    /// only in a both-engines build and self-skips if either fp32 model isn't
    /// cached, so it's safe under CI / offline.
    #[cfg(feature = "onnxruntime")]
    #[tokio::test]
    async fn cosine_matches_onnx_fp32() {
        use crate::embeddings::{OnnxProvider, ONNX_ALL_MINILM};
        let (Some(tract), Some(onnx)) = (
            TractEmbeddingProvider::try_new(),
            OnnxProvider::try_with_model(ONNX_ALL_MINILM),
        ) else {
            eprintln!("Skipping: tract or fp32 ONNX model unavailable");
            return;
        };
        for text in [
            "EngramDB stores project-scoped memory for coding agents.",
            "Rust provides memory safety without a garbage collector.",
        ] {
            let a = tract.embed(text).await.unwrap();
            let b = onnx.embed(text).await.unwrap();
            // Both providers L2-normalize, so dot product is cosine similarity.
            let cos: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            assert!(
                cos >= 0.999,
                "tract vs ONNX fp32 cosine = {cos} for {text:?}"
            );
        }
    }
}
