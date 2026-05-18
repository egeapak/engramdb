//! T5-Small ONNX-based title generator for abstractive summarization.
//!
//! Downloads the T5-small model from HuggingFace Hub on first use (~60MB quantized),
//! caches it in the unified EngramDB model cache directory, and runs inference
//! via ONNX Runtime. All inference is wrapped in `spawn_blocking` for async
//! compatibility since ONNX inference is CPU-bound.
//!
//! The model generates a short abstractive summary from the input text,
//! which is then truncated to a few words for use as a title.

use super::TitleGenerator;
use anyhow::{Context, Result};
use async_trait::async_trait;
use ort::session::{builder::GraphOptimizationLevel, Session};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

/// HuggingFace repository for quantized T5-small ONNX model.
const MODEL_REPO: &str = "ArsenyParamonov/t5-small-onnx";

/// Encoder model file within the repository.
const ENCODER_FILE: &str = "encoder_model.onnx";

/// Decoder model file within the repository.
const DECODER_FILE: &str = "decoder_model.onnx";

/// Tokenizer file within the repository.
const TOKENIZER_FILE: &str = "tokenizer.json";

/// Maximum number of input tokens.
const MAX_INPUT_TOKENS: usize = 128;

/// Maximum number of output tokens to generate.
const MAX_OUTPUT_TOKENS: usize = 16;

/// T5-small ONNX-based title generator.
///
/// Uses encoder-decoder architecture: the encoder processes the input text,
/// and the decoder generates title tokens autoregressively.
pub struct T5TitleGenerator {
    encoder: Arc<Mutex<Session>>,
    decoder: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl T5TitleGenerator {
    /// Create a new T5 title generator.
    ///
    /// Downloads the model and tokenizer from HuggingFace Hub if not cached.
    pub fn new() -> Result<Self> {
        Self::with_repo(MODEL_REPO)
    }

    /// Create with a custom HuggingFace repository.
    pub fn with_repo(repo: &str) -> Result<Self> {
        let (encoder_path, decoder_path, tokenizer_path) = download_model_files(repo)?;

        let encoder = crate::onnx_ep::apply_execution_providers(Session::builder()?)?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_file(&encoder_path)
            .context("Failed to load T5 encoder ONNX model")?;

        let decoder = crate::onnx_ep::apply_execution_providers(Session::builder()?)?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(1)?
            .commit_from_file(&decoder_path)
            .context("Failed to load T5 decoder ONNX model")?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load T5 tokenizer: {}", e))?;

        Ok(Self {
            encoder: Arc::new(Mutex::new(encoder)),
            decoder: Arc::new(Mutex::new(decoder)),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Try to create, returning None if unavailable.
    pub fn try_new() -> Option<Self> {
        match Self::new() {
            Ok(gen) => Some(gen),
            Err(e) => {
                tracing::warn!("T5 title generator unavailable: {}", e);
                None
            }
        }
    }
}

/// Download T5 model files from HuggingFace Hub.
fn download_model_files(model_repo: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let cache_dir =
        crate::storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(cache_dir)
        .build()
        .context("Failed to initialize HuggingFace API for T5")?;
    let repo = api.model(model_repo.to_string());

    let encoder_path = repo
        .get(ENCODER_FILE)
        .context("Failed to download T5 encoder model")?;
    let decoder_path = repo
        .get(DECODER_FILE)
        .context("Failed to download T5 decoder model")?;
    let tokenizer_path = repo
        .get(TOKENIZER_FILE)
        .context("Failed to download T5 tokenizer")?;

    Ok((encoder_path, decoder_path, tokenizer_path))
}

/// Run T5 encoder to produce hidden states.
fn encode(
    encoder: &mut Session,
    tokenizer: &Tokenizer,
    text: &str,
) -> Result<(Vec<f32>, Vec<usize>)> {
    // Prepend task prefix for summarization
    let input = format!("summarize: {}", text);

    let encoding = tokenizer
        .encode(input.as_str(), true)
        .map_err(|e| anyhow::anyhow!("T5 tokenization failed: {}", e))?;

    // Truncate to max input tokens
    let ids: Vec<i64> = encoding
        .get_ids()
        .iter()
        .take(MAX_INPUT_TOKENS)
        .map(|&id| id as i64)
        .collect();
    let mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .take(MAX_INPUT_TOKENS)
        .map(|&m| m as i64)
        .collect();
    let length = ids.len();

    let ids_tensor = ort::value::TensorRef::from_array_view(([1usize, length], ids.as_slice()))?;
    let mask_tensor = ort::value::TensorRef::from_array_view(([1usize, length], mask.as_slice()))?;

    let inputs: Vec<(std::borrow::Cow<str>, ort::session::SessionInputValue)> = vec![
        ("input_ids".into(), ids_tensor.into()),
        ("attention_mask".into(), mask_tensor.into()),
    ];

    let outputs = encoder.run(inputs)?;

    // Extract encoder hidden states (batch=1, seq_len, hidden_dim)
    let (_shape, hidden_slice) = outputs[0].try_extract_tensor::<f32>()?;
    let data = hidden_slice.to_vec();

    Ok((data, vec![1, length]))
}

/// Greedy decode: generate tokens one at a time using the decoder.
fn greedy_decode(
    decoder: &mut Session,
    tokenizer: &Tokenizer,
    encoder_hidden: &[f32],
    encoder_shape: &[usize],
) -> Result<String> {
    // T5 uses pad_token_id (0) as decoder start token
    let mut generated_ids: Vec<i64> = vec![0];
    let eos_token_id: i64 = 1; // T5 EOS token

    for _ in 0..MAX_OUTPUT_TOKENS {
        let dec_len = generated_ids.len();
        let dec_tensor =
            ort::value::TensorRef::from_array_view(([1usize, dec_len], generated_ids.as_slice()))?;

        let enc_tensor =
            ort::value::TensorRef::from_array_view((encoder_shape.to_vec(), encoder_hidden))?;

        let enc_mask = vec![1i64; encoder_shape[1]];
        let mask_tensor = ort::value::TensorRef::from_array_view((
            [1usize, encoder_shape[1]],
            enc_mask.as_slice(),
        ))?;

        let inputs: Vec<(std::borrow::Cow<str>, ort::session::SessionInputValue)> = vec![
            ("input_ids".into(), dec_tensor.into()),
            ("encoder_hidden_states".into(), enc_tensor.into()),
            ("encoder_attention_mask".into(), mask_tensor.into()),
        ];

        let outputs = decoder.run(inputs)?;

        // Get logits: (batch=1, dec_len, vocab_size)
        let (_logits_shape, logits_data) = outputs[0].try_extract_tensor::<f32>()?;

        // Get vocab size from the last dimension
        let total = logits_data.len();
        let vocab_size = total / dec_len; // batch=1

        // Take logits for last token position
        let last_token_logits = &logits_data[(dec_len - 1) * vocab_size..dec_len * vocab_size];

        // Greedy: pick argmax
        let next_id = last_token_logits
            .iter()
            .enumerate()
            .max_by(|(_, a): &(usize, &f32), (_, b): &(usize, &f32)| {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(idx, _)| idx as i64)
            .unwrap_or(eos_token_id);

        if next_id == eos_token_id {
            break;
        }

        generated_ids.push(next_id);
    }

    // Decode tokens (skip the initial pad token)
    let token_ids: Vec<u32> = generated_ids.iter().skip(1).map(|&id| id as u32).collect();
    let decoded = tokenizer
        .decode(&token_ids, true)
        .map_err(|e| anyhow::anyhow!("T5 decoding failed: {}", e))?;

    Ok(decoded.trim().to_string())
}

/// Generate a title synchronously (blocking).
fn generate_sync(
    encoder: &Mutex<Session>,
    decoder: &Mutex<Session>,
    tokenizer: &Tokenizer,
    text: &str,
) -> Result<String> {
    let mut enc_session = encoder
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire T5 encoder lock: {}", e))?;

    let (hidden, shape) = encode(&mut enc_session, tokenizer, text)?;
    drop(enc_session); // Release encoder lock before decoding

    let mut dec_session = decoder
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire T5 decoder lock: {}", e))?;

    let raw_title = greedy_decode(&mut dec_session, tokenizer, &hidden, &shape)?;

    // Truncate to a few words for a title
    let words: Vec<&str> = raw_title.split_whitespace().take(6).collect();
    Ok(words.join(" "))
}

#[async_trait]
impl TitleGenerator for T5TitleGenerator {
    async fn generate(&self, text: &str) -> Result<String> {
        let encoder = Arc::clone(&self.encoder);
        let decoder = Arc::clone(&self.decoder);
        let tokenizer = Arc::clone(&self.tokenizer);
        let text = text.to_string();

        tokio::task::spawn_blocking(move || generate_sync(&encoder, &decoder, &tokenizer, &text))
            .await
            .context("T5 title generation task panicked")?
    }
}
