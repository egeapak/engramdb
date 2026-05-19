//! ONNX-based NLI provider using cross-encoder/nli-deberta-v3-xsmall.
//!
//! Downloads the model and tokenizer from HuggingFace Hub, caches them locally,
//! and runs inference via ONNX Runtime. All inference is wrapped in
//! `spawn_blocking` for async compatibility since ONNX inference is CPU-bound.

use super::{NliProvider, NliResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use ndarray::Ix2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

/// Default HuggingFace repository ID for the NLI model.
#[cfg(test)]
const DEFAULT_MODEL_REPO: &str = "cross-encoder/nli-deberta-v3-xsmall";

/// Default ONNX model file path within the repository (fp32).
const MODEL_FILE: &str = "onnx/model.onnx";

/// Tokenizer file path within the repository.
const TOKENIZER_FILE: &str = "tokenizer.json";

/// Location of an NLI cross-encoder ONNX model on HuggingFace Hub. The
/// label ordering must be {0: contradiction, 1: entailment, 2: neutral}
/// (matches the [`IDX_*`](IDX_CONTRADICTION) constants); the Xenova mirror
/// preserves the original `cross-encoder` ordering.
#[derive(Debug, Clone, Copy)]
pub struct NliModelSpec {
    /// HuggingFace repo id.
    pub repo: &'static str,
    /// ONNX model path within the repo.
    pub model_file: &'static str,
    /// Tokenizer JSON path within the repo.
    pub tokenizer_file: &'static str,
}

/// `cross-encoder/nli-deberta-v3-xsmall` fp32 (the historical default,
/// ~271 MB on disk).
pub const NLI_DEBERTA_XSMALL: NliModelSpec = NliModelSpec {
    repo: "cross-encoder/nli-deberta-v3-xsmall",
    model_file: "onnx/model.onnx",
    tokenizer_file: "tokenizer.json",
};

/// `Xenova/nli-deberta-v3-xsmall` int8-quantized (~83 MB vs ~271 MB;
/// identical id2label ordering). ~2× faster, ~3.7× less RAM.
pub const NLI_DEBERTA_XSMALL_Q: NliModelSpec = NliModelSpec {
    repo: "Xenova/nli-deberta-v3-xsmall",
    model_file: "onnx/model_quantized.onnx",
    tokenizer_file: "tokenizer.json",
};

/// Default NLI model (single source of truth, mirrors `DEFAULT_T5_MODEL`).
/// int8 chosen after the Lever D A/B: ~2× faster, ~3.7× less RAM, same
/// label ordering, no quality regression. `NliConfig::default().model`
/// must equal `DEFAULT_NLI_MODEL.repo`.
pub const DEFAULT_NLI_MODEL: NliModelSpec = NLI_DEBERTA_XSMALL_Q;

/// Model output label ordering:
/// - Index 0: contradiction
/// - Index 1: entailment
/// - Index 2: neutral
const IDX_CONTRADICTION: usize = 0;
const IDX_ENTAILMENT: usize = 1;
const IDX_NEUTRAL: usize = 2;

/// ONNX-based NLI provider using DeBERTa v3 xsmall cross-encoder.
///
/// Classifies sentence pairs as entailment, neutral, or contradiction.
/// The model and tokenizer are downloaded from HuggingFace Hub on first use
/// and cached locally.
pub struct OnnxNliProvider {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl OnnxNliProvider {
    /// Create a new ONNX NLI provider with the specified model repository.
    ///
    /// Downloads the model and tokenizer from HuggingFace Hub if not cached.
    /// The files are cached in the unified EngramDB model cache directory
    /// (`<cache_dir>/engramdb/models/`).
    pub fn new(model_repo: &str) -> Result<Self> {
        Self::new_on(model_repo, crate::onnx_ep::default_backend())
    }

    /// Create a new ONNX NLI provider on an explicit execution backend.
    ///
    /// Used by the benchmark suite to compare CPU vs Core ML on identical
    /// workloads; production code should use [`OnnxNliProvider::new`].
    pub fn new_on(model_repo: &str, backend: crate::onnx_ep::Backend) -> Result<Self> {
        // Map known repos to the right ONNX file so the string-based
        // config API still selects the int8 model for the default repo.
        // Unknown (user-custom) repos keep the historical fp32 defaults.
        let (model_file, tokenizer_file) = if model_repo == NLI_DEBERTA_XSMALL_Q.repo {
            (
                NLI_DEBERTA_XSMALL_Q.model_file,
                NLI_DEBERTA_XSMALL_Q.tokenizer_file,
            )
        } else if model_repo == NLI_DEBERTA_XSMALL.repo {
            (
                NLI_DEBERTA_XSMALL.model_file,
                NLI_DEBERTA_XSMALL.tokenizer_file,
            )
        } else {
            (MODEL_FILE, TOKENIZER_FILE)
        };
        Self::build(model_repo, model_file, tokenizer_file, backend)
    }

    /// Create from an explicit [`NliModelSpec`] on an explicit backend.
    ///
    /// Used by the benchmark suite to A/B model sources (fp32 vs int8) and
    /// backends; production code should use [`OnnxNliProvider::new`].
    pub fn with_spec_on(spec: &NliModelSpec, backend: crate::onnx_ep::Backend) -> Result<Self> {
        Self::build(spec.repo, spec.model_file, spec.tokenizer_file, backend)
    }

    /// Try to create from an explicit [`NliModelSpec`] on an explicit
    /// backend, returning None if unavailable.
    pub fn try_with_spec_on(spec: &NliModelSpec, backend: crate::onnx_ep::Backend) -> Option<Self> {
        Self::with_spec_on(spec, backend).ok()
    }

    fn build(
        repo: &str,
        model_file: &str,
        tokenizer_file: &str,
        backend: crate::onnx_ep::Backend,
    ) -> Result<Self> {
        let (model_path, tokenizer_path) = download_model_files(repo, model_file, tokenizer_file)?;

        let builder = crate::onnx_ep::apply_backend(Session::builder()?, backend)?;
        let session = builder
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(crate::onnx_ep::intra_threads())?
            .commit_from_file(&model_path)
            .context("Failed to load NLI ONNX model")?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load NLI tokenizer: {}", e))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Try to create a new provider with the specified model, returning None if unavailable.
    ///
    /// Useful for graceful degradation when NLI is optional.
    pub fn try_new(model_repo: &str) -> Option<Self> {
        match Self::new(model_repo) {
            Ok(provider) => Some(provider),
            Err(e) => {
                tracing::warn!("NLI provider unavailable: {}", e);
                None
            }
        }
    }

    /// Try to create a provider on an explicit backend, returning None if
    /// unavailable.
    pub fn try_new_on(model_repo: &str, backend: crate::onnx_ep::Backend) -> Option<Self> {
        Self::new_on(model_repo, backend).ok()
    }
}

/// Download model and tokenizer files from HuggingFace Hub.
///
/// Returns `(model_path, tokenizer_path)`. Files are cached in the unified
/// EngramDB model cache directory (`<cache_dir>/engramdb/models/`) and reused
/// on subsequent calls.
fn download_model_files(
    model_repo: &str,
    model_file: &str,
    tokenizer_file: &str,
) -> Result<(PathBuf, PathBuf)> {
    let cache_dir =
        crate::storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(cache_dir)
        .build()
        .context("Failed to initialize HuggingFace API")?;
    let repo = api.model(model_repo.to_string());

    let model_path = repo
        .get(model_file)
        .context("Failed to download NLI ONNX model")?;
    let tokenizer_path = repo
        .get(tokenizer_file)
        .context("Failed to download NLI tokenizer")?;

    Ok((model_path, tokenizer_path))
}

/// Run NLI inference on a single sentence pair with an already-locked session.
fn classify_one(
    session: &mut Session,
    tokenizer: &Tokenizer,
    premise: &str,
    hypothesis: &str,
) -> Result<NliResult> {
    let encoding = tokenizer
        .encode((premise, hypothesis), true)
        .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

    let length = encoding.len();
    let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

    let ids_tensor =
        ort::value::TensorRef::from_array_view(([1usize, length], input_ids.as_slice()))?;
    let mask_tensor =
        ort::value::TensorRef::from_array_view(([1usize, length], attention_mask.as_slice()))?;
    let type_tensor =
        ort::value::TensorRef::from_array_view(([1usize, length], token_type_ids.as_slice()))?;

    let mut inputs: Vec<(std::borrow::Cow<str>, ort::session::SessionInputValue)> = vec![
        ("input_ids".into(), ids_tensor.into()),
        ("attention_mask".into(), mask_tensor.into()),
    ];

    let has_token_type_ids = session
        .inputs()
        .iter()
        .any(|i| i.name() == "token_type_ids");
    if has_token_type_ids {
        inputs.push(("token_type_ids".into(), type_tensor.into()));
    }

    let outputs = session.run(inputs)?;

    let logits = outputs[0]
        .try_extract_array::<f32>()?
        .into_dimensionality::<Ix2>()
        .context("Expected 2D logits output")?;

    let row = logits.row(0);
    let slice = row
        .as_slice()
        .ok_or_else(|| anyhow::anyhow!("Expected contiguous logits array from NLI model"))?;
    let probs = softmax(slice);

    Ok(NliResult::from_probs(
        probs[IDX_ENTAILMENT],
        probs[IDX_NEUTRAL],
        probs[IDX_CONTRADICTION],
    ))
}

/// Run NLI inference on a single sentence pair (blocking). Locks the session mutex.
fn classify_sync(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    premise: &str,
    hypothesis: &str,
) -> Result<NliResult> {
    let mut session = session
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire NLI session lock: {}", e))?;
    classify_one(&mut session, tokenizer, premise, hypothesis)
}

/// Run NLI inference on multiple sentence pairs (blocking).
///
/// Acquires the session lock once and processes all pairs sequentially,
/// avoiding per-pair lock overhead.
fn classify_batch_sync(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    pairs: &[(&str, &str)],
) -> Result<Vec<NliResult>> {
    let mut session = session
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire NLI session lock: {}", e))?;
    pairs
        .iter()
        .map(|(premise, hypothesis)| classify_one(&mut session, tokenizer, premise, hypothesis))
        .collect()
}

/// Compute softmax over a slice of logits.
fn softmax(logits: &[f32]) -> Vec<f32> {
    let max_val = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exp_vals: Vec<f32> = logits.iter().map(|&x| (x - max_val).exp()).collect();
    let sum: f32 = exp_vals.iter().sum();
    exp_vals.iter().map(|&x| x / sum).collect()
}

#[async_trait]
impl NliProvider for OnnxNliProvider {
    async fn classify(&self, premise: &str, hypothesis: &str) -> Result<NliResult> {
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let premise = premise.to_string();
        let hypothesis = hypothesis.to_string();

        tokio::task::spawn_blocking(move || {
            classify_sync(&session, &tokenizer, &premise, &hypothesis)
        })
        .await
        .context("NLI classify task panicked")?
    }

    async fn classify_batch(&self, pairs: &[(&str, &str)]) -> Result<Vec<NliResult>> {
        let session = Arc::clone(&self.session);
        let tokenizer = Arc::clone(&self.tokenizer);
        let pairs_owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(p, h)| (p.to_string(), h.to_string()))
            .collect();

        tokio::task::spawn_blocking(move || {
            let pair_refs: Vec<(&str, &str)> = pairs_owned
                .iter()
                .map(|(p, h)| (p.as_str(), h.as_str()))
                .collect();
            classify_batch_sync(&session, &tokenizer, &pair_refs)
        })
        .await
        .context("NLI classify_batch task panicked")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nli::NliLabel;
    use std::sync::LazyLock;

    /// Shared NLI provider across all tests in this module to avoid loading
    /// the ~400MB ONNX model once per test (which causes OOM when parallel).
    static SHARED_PROVIDER: LazyLock<Option<OnnxNliProvider>> =
        LazyLock::new(|| OnnxNliProvider::try_new(DEFAULT_MODEL_REPO));

    /// Returns the shared provider, or None if unavailable (for graceful skip).
    fn try_provider() -> Option<&'static OnnxNliProvider> {
        let provider = SHARED_PROVIDER.as_ref();
        if provider.is_none() {
            eprintln!("Skipping: NLI model not available");
        }
        provider
    }

    #[test]
    fn test_softmax() {
        let logits = vec![1.0, 2.0, 3.0];
        let probs = softmax(&logits);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "softmax should sum to 1.0");
        assert!(probs[2] > probs[1]);
        assert!(probs[1] > probs[0]);
    }

    #[test]
    fn test_softmax_single() {
        let probs = softmax(&[5.0]);
        assert_eq!(probs.len(), 1);
        assert!((probs[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_softmax_equal() {
        let probs = softmax(&[1.0, 1.0, 1.0]);
        for p in &probs {
            assert!((*p - 1.0 / 3.0).abs() < 1e-5);
        }
    }

    #[test]
    fn test_provider_creation() {
        // This test requires model download; skip gracefully if unavailable
        let _ = try_provider();
    }

    #[tokio::test]
    async fn test_classify_contradiction() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify(
                "Use PostgreSQL for the database",
                "Use SQLite for the database",
            )
            .await
            .unwrap();

        // These sentences should be classified as contradiction
        assert!(
            result.contradiction > 0.3,
            "Expected high contradiction score, got {}",
            result.contradiction
        );
    }

    #[tokio::test]
    async fn test_classify_batch() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let pairs = vec![
            ("The sky is blue", "The sky is not blue"),
            ("A dog runs fast", "An animal is moving"),
        ];

        let results = provider.classify_batch(&pairs).await.unwrap();
        assert_eq!(results.len(), 2);

        // First pair should lean contradiction
        assert!(results[0].contradiction > results[0].entailment);
        // Second pair should lean entailment
        assert!(results[1].entailment > results[1].contradiction);
    }

    #[tokio::test]
    async fn test_softmax_sums_to_one() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify("premise text", "hypothesis text")
            .await
            .unwrap();

        let sum = result.entailment + result.neutral + result.contradiction;
        assert!(
            (sum - 1.0).abs() < 1e-4,
            "Probabilities should sum to 1.0, got {}",
            sum
        );
    }

    #[tokio::test]
    async fn test_label_entailment() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify("A person is eating pizza", "A person is having a meal")
            .await
            .unwrap();

        assert_eq!(
            result.label,
            NliLabel::Entailment,
            "Expected Entailment label, got {:?} (e={}, n={}, c={})",
            result.label,
            result.entailment,
            result.neutral,
            result.contradiction
        );
        assert!(
            result.entailment > 0.5,
            "Expected entailment > 0.5, got {}",
            result.entailment
        );
    }

    #[tokio::test]
    async fn test_label_contradiction() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify("The sky is blue", "The sky is not blue")
            .await
            .unwrap();

        assert_eq!(
            result.label,
            NliLabel::Contradiction,
            "Expected Contradiction label, got {:?} (e={}, n={}, c={})",
            result.label,
            result.entailment,
            result.neutral,
            result.contradiction
        );
        assert!(
            result.contradiction > 0.5,
            "Expected contradiction > 0.5, got {}",
            result.contradiction
        );
    }

    #[tokio::test]
    async fn test_label_neutral() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify("A cat sits on a mat", "The weather is sunny today")
            .await
            .unwrap();

        assert_eq!(
            result.label,
            NliLabel::Neutral,
            "Expected Neutral label, got {:?} (e={}, n={}, c={})",
            result.label,
            result.entailment,
            result.neutral,
            result.contradiction
        );
        assert!(
            result.neutral > 0.5,
            "Expected neutral > 0.5, got {}",
            result.neutral
        );
    }

    #[tokio::test]
    async fn test_entailment_is_asymmetric() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        // Forward: "A dog is running" entails "An animal is moving"
        let forward = provider
            .classify("A dog is running", "An animal is moving")
            .await
            .unwrap();
        assert_eq!(
            forward.label,
            NliLabel::Entailment,
            "Forward should be Entailment, got {:?} (e={}, n={}, c={})",
            forward.label,
            forward.entailment,
            forward.neutral,
            forward.contradiction
        );

        // Reverse: "An animal is moving" does NOT entail "A dog is running"
        let reverse = provider
            .classify("An animal is moving", "A dog is running")
            .await
            .unwrap();
        assert_ne!(
            reverse.label,
            NliLabel::Entailment,
            "Reverse should NOT be Entailment, got {:?} (e={}, n={}, c={})",
            reverse.label,
            reverse.entailment,
            reverse.neutral,
            reverse.contradiction
        );
    }

    #[tokio::test]
    async fn test_identical_sentences_entail() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify(
                "The server crashed at midnight",
                "The server crashed at midnight",
            )
            .await
            .unwrap();

        assert_eq!(
            result.label,
            NliLabel::Entailment,
            "Identical sentences should be Entailment, got {:?} (e={}, n={}, c={})",
            result.label,
            result.entailment,
            result.neutral,
            result.contradiction
        );
        assert!(
            result.entailment > 0.8,
            "Identical sentences should have entailment > 0.8, got {}",
            result.entailment
        );
    }

    #[tokio::test]
    async fn test_antonym_contradiction() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let result = provider
            .classify("The restaurant is open", "The restaurant is closed")
            .await
            .unwrap();

        assert_eq!(
            result.label,
            NliLabel::Contradiction,
            "Antonym pair should be Contradiction, got {:?} (e={}, n={}, c={})",
            result.label,
            result.entailment,
            result.neutral,
            result.contradiction
        );
        assert!(
            result.contradiction > 0.5,
            "Expected contradiction > 0.5 for antonym pair, got {}",
            result.contradiction
        );
    }

    #[tokio::test]
    async fn test_batch_matches_individual() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let pairs: Vec<(&str, &str)> = vec![
            ("A person is eating pizza", "A person is having a meal"),
            ("The sky is blue", "The sky is not blue"),
            ("A cat sits on a mat", "The weather is sunny today"),
        ];

        // Classify individually
        let mut individual_results = Vec::new();
        for (premise, hypothesis) in &pairs {
            let result = provider.classify(premise, hypothesis).await.unwrap();
            individual_results.push(result);
        }

        // Classify as batch
        let batch_results = provider.classify_batch(&pairs).await.unwrap();

        for (i, (ind, batch)) in individual_results.iter().zip(&batch_results).enumerate() {
            assert_eq!(
                ind.label, batch.label,
                "Pair {}: individual label {:?} != batch label {:?}",
                i, ind.label, batch.label
            );
            assert!(
                (ind.entailment - batch.entailment).abs() < 1e-6,
                "Pair {}: entailment mismatch: {} vs {}",
                i,
                ind.entailment,
                batch.entailment
            );
            assert!(
                (ind.neutral - batch.neutral).abs() < 1e-6,
                "Pair {}: neutral mismatch: {} vs {}",
                i,
                ind.neutral,
                batch.neutral
            );
            assert!(
                (ind.contradiction - batch.contradiction).abs() < 1e-6,
                "Pair {}: contradiction mismatch: {} vs {}",
                i,
                ind.contradiction,
                batch.contradiction
            );
        }
    }

    #[tokio::test]
    async fn test_batch_empty() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        let pairs: Vec<(&str, &str)> = vec![];
        let results = provider.classify_batch(&pairs).await.unwrap();
        assert!(
            results.is_empty(),
            "Empty batch should return empty results, got {} results",
            results.len()
        );
    }

    #[tokio::test]
    async fn test_empty_strings_do_not_panic() {
        let provider = match try_provider() {
            Some(p) => p,
            None => return,
        };

        // Empty premise and hypothesis should not panic
        let result = provider.classify("", "").await.unwrap();
        let sum = result.entailment + result.neutral + result.contradiction;
        assert!(
            (sum - 1.0).abs() < 1e-4,
            "Probabilities should still sum to 1.0, got {}",
            sum
        );

        // One empty, one non-empty
        let result = provider.classify("", "A person is eating").await.unwrap();
        let sum = result.entailment + result.neutral + result.contradiction;
        assert!(
            (sum - 1.0).abs() < 1e-4,
            "Probabilities should still sum to 1.0, got {}",
            sum
        );
    }

    /// Verify that a poisoned mutex returns Err instead of panicking.
    ///
    /// This tests the pattern change from `.expect()` to `.map_err()` — we poison
    /// a plain `Mutex<u32>` to prove `map_err` catches poisoning gracefully.
    #[test]
    fn test_poisoned_mutex_returns_error_not_panic() {
        use std::sync::Mutex;

        let mutex = Mutex::new(42u32);

        // Poison the mutex by panicking while holding the lock
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("intentional panic to poison mutex");
        }));

        // Verify that `.lock()` on a poisoned mutex returns Err
        assert!(mutex.lock().is_err(), "mutex should be poisoned");

        // Verify our pattern converts it to an anyhow::Error instead of panicking
        let result: anyhow::Result<u32> = mutex
            .lock()
            .map(|guard| *guard)
            .map_err(|e| anyhow::anyhow!("Failed to acquire lock: {}", e));

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to acquire lock"));
    }
}
