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

/// HuggingFace repository ID for the NLI model.
const MODEL_REPO: &str = "cross-encoder/nli-deberta-v3-xsmall";

/// ONNX model file path within the repository.
const MODEL_FILE: &str = "onnx/model.onnx";

/// Tokenizer file path within the repository.
const TOKENIZER_FILE: &str = "tokenizer.json";

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
    /// Create a new ONNX NLI provider.
    ///
    /// Downloads the model and tokenizer from HuggingFace Hub if not cached.
    /// The files are cached in the default HuggingFace cache directory
    /// (`~/.cache/huggingface/hub/`).
    pub fn new() -> Result<Self> {
        let (model_path, tokenizer_path) = download_model_files()?;

        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_file(&model_path)
            .context("Failed to load NLI ONNX model")?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load NLI tokenizer: {}", e))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Try to create a new provider, returning None if unavailable.
    ///
    /// Useful for graceful degradation when NLI is optional.
    pub fn try_new() -> Option<Self> {
        match Self::new() {
            Ok(provider) => Some(provider),
            Err(e) => {
                tracing::warn!("NLI provider unavailable: {}", e);
                None
            }
        }
    }
}

/// Download model and tokenizer files from HuggingFace Hub.
///
/// Returns `(model_path, tokenizer_path)`. Files are cached and reused
/// on subsequent calls.
fn download_model_files() -> Result<(PathBuf, PathBuf)> {
    let api = hf_hub::api::sync::Api::new().context("Failed to initialize HuggingFace API")?;
    let repo = api.model(MODEL_REPO.to_string());

    let model_path = repo
        .get(MODEL_FILE)
        .context("Failed to download NLI ONNX model")?;
    let tokenizer_path = repo
        .get(TOKENIZER_FILE)
        .context("Failed to download NLI tokenizer")?;

    Ok((model_path, tokenizer_path))
}

/// Run NLI inference on a single sentence pair (blocking).
fn classify_sync(
    session: &Mutex<Session>,
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

    let mut session = session.lock().expect("NLI session mutex poisoned");
    let outputs = session.run(ort::inputs![
        "input_ids" => ids_tensor,
        "attention_mask" => mask_tensor,
        "token_type_ids" => type_tensor
    ])?;

    let logits = outputs[0]
        .try_extract_array::<f32>()?
        .into_dimensionality::<Ix2>()
        .context("Expected 2D logits output")?;

    let row = logits.row(0);
    let probs = softmax(row.as_slice().unwrap());

    Ok(NliResult::from_probs(
        probs[IDX_ENTAILMENT],
        probs[IDX_NEUTRAL],
        probs[IDX_CONTRADICTION],
    ))
}

/// Run NLI inference on multiple sentence pairs (blocking).
fn classify_batch_sync(
    session: &Mutex<Session>,
    tokenizer: &Tokenizer,
    pairs: &[(&str, &str)],
) -> Result<Vec<NliResult>> {
    // For simplicity and to avoid padding complexity, process each pair individually.
    // The model is small enough that per-pair inference is fast.
    pairs
        .iter()
        .map(|(premise, hypothesis)| classify_sync(session, tokenizer, premise, hypothesis))
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
        let provider = OnnxNliProvider::try_new();
        if provider.is_none() {
            eprintln!("Skipping: NLI model not available");
            return;
        }
        assert!(provider.is_some());
    }

    #[tokio::test]
    async fn test_classify_contradiction() {
        let provider = match OnnxNliProvider::try_new() {
            Some(p) => p,
            None => {
                eprintln!("Skipping: NLI model not available");
                return;
            }
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
    async fn test_classify_entailment() {
        let provider = match OnnxNliProvider::try_new() {
            Some(p) => p,
            None => {
                eprintln!("Skipping: NLI model not available");
                return;
            }
        };

        let result = provider
            .classify("A person is eating pizza", "A person is having a meal")
            .await
            .unwrap();

        assert!(
            result.entailment > 0.3,
            "Expected high entailment score, got {}",
            result.entailment
        );
    }

    #[tokio::test]
    async fn test_classify_neutral() {
        let provider = match OnnxNliProvider::try_new() {
            Some(p) => p,
            None => {
                eprintln!("Skipping: NLI model not available");
                return;
            }
        };

        let result = provider
            .classify("A cat sits on a mat", "The weather is sunny today")
            .await
            .unwrap();

        assert!(
            result.neutral > 0.3,
            "Expected high neutral score, got {}",
            result.neutral
        );
    }

    #[tokio::test]
    async fn test_classify_batch() {
        let provider = match OnnxNliProvider::try_new() {
            Some(p) => p,
            None => {
                eprintln!("Skipping: NLI model not available");
                return;
            }
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
        let provider = match OnnxNliProvider::try_new() {
            Some(p) => p,
            None => {
                eprintln!("Skipping: NLI model not available");
                return;
            }
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
}
