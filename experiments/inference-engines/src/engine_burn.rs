//! Burn engine: a BERT encoder built from Burn 0.21's stock modules
//! (`Embedding`, `LayerNorm`, `TransformerEncoder`), fp32 weights from
//! `sentence-transformers/all-MiniLM-L12-v2` loaded with `burn-store`.
//!
//! Adapted from `tracel-ai/models` `minilm-burn` (which targets Burn's
//! unreleased main branch, without the `Backend` generic). Generic over the
//! backend so the Flex and NdArray builds share one code path.

use crate::common::{Batch, Engine, HIDDEN};
use anyhow::{anyhow, Result};
use burn::module::Module;
use burn::nn::transformer::{TransformerEncoder, TransformerEncoderConfig, TransformerEncoderInput};
use burn::nn::{Embedding, EmbeddingConfig, LayerNorm, LayerNormConfig};
use burn::prelude::*;
use burn_store::{KeyRemapper, ModuleSnapshot, PyTorchToBurnAdapter, SafetensorsStore};
use serde::Deserialize;
use std::path::Path;

#[cfg(feature = "burn-flex")]
pub type B = burn::backend::Flex;
#[cfg(all(feature = "burn-ndarray", not(feature = "burn-flex")))]
pub type B = burn::backend::NdArray;

#[derive(Deserialize)]
struct HfConfig {
    hidden_size: usize,
    num_attention_heads: usize,
    num_hidden_layers: usize,
    intermediate_size: usize,
    vocab_size: usize,
    max_position_embeddings: usize,
    type_vocab_size: usize,
    layer_norm_eps: f64,
}

#[derive(Module, Debug)]
pub struct Embeddings<B: Backend> {
    word_embeddings: Embedding<B>,
    position_embeddings: Embedding<B>,
    token_type_embeddings: Embedding<B>,
    layer_norm: LayerNorm<B>,
}

#[derive(Module, Debug)]
pub struct Bert<B: Backend> {
    embeddings: Embeddings<B>,
    encoder: TransformerEncoder<B>,
}

impl<B: Backend> Bert<B> {
    fn init(c: &HfConfig, device: &burn::tensor::Device<B>) -> Self {
        let embeddings = Embeddings {
            word_embeddings: EmbeddingConfig::new(c.vocab_size, c.hidden_size).init(device),
            position_embeddings: EmbeddingConfig::new(c.max_position_embeddings, c.hidden_size)
                .init(device),
            token_type_embeddings: EmbeddingConfig::new(c.type_vocab_size, c.hidden_size)
                .init(device),
            layer_norm: LayerNormConfig::new(c.hidden_size)
                .with_epsilon(c.layer_norm_eps)
                .init(device),
        };
        let encoder = TransformerEncoderConfig::new(
            c.hidden_size,
            c.intermediate_size,
            c.num_attention_heads,
            c.num_hidden_layers,
        )
        .with_dropout(0.0)
        .with_norm_first(false) // BERT is post-LayerNorm
        .with_quiet_softmax(false)
        // Burn's default is 1e-5; BERT uses 1e-12. Leaving the default in
        // place silently changes every LayerNorm.
        .with_layer_norm_eps(c.layer_norm_eps)
        .init(device);
        Self {
            embeddings,
            encoder,
        }
    }

    fn forward(&self, ids: Tensor<B, 2, Int>, types: Tensor<B, 2, Int>, mask: Tensor<B, 2, Int>) -> Tensor<B, 3> {
        let [batch, seq] = ids.dims();
        let device = ids.device();
        let pos = Tensor::<B, 1, Int>::arange(0..seq as i64, &device)
            .reshape([1, seq])
            .expand([batch, seq]);
        let e = &self.embeddings;
        let x = e.word_embeddings.forward(ids)
            + e.position_embeddings.forward(pos)
            + e.token_type_embeddings.forward(types);
        let x = e.layer_norm.forward(x);
        // Burn's mask_pad is `true` where the token is padding.
        let mask_pad = mask.equal_elem(0);
        self.encoder
            .forward(TransformerEncoderInput::new(x).mask_pad(mask_pad))
    }
}

pub struct BurnEngine {
    model: Bert<B>,
    device: burn::tensor::Device<B>,
}

impl BurnEngine {
    pub fn load(dir: &Path) -> Result<Self> {
        let device = Default::default();
        let config: HfConfig =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let mut model = Bert::<B>::init(&config, &device);

        // HuggingFace BERT names -> Burn TransformerEncoder names. Same table
        // as minilm-burn; order matters (specific patterns first).
        let remap = KeyRemapper::from_patterns(vec![
            ("^bert\\.(.+)", "$1"),
            ("encoder\\.layer\\.([0-9]+)", "encoder.layers.$1"),
            ("attention\\.self\\.query", "mha.query"),
            ("attention\\.self\\.key", "mha.key"),
            ("attention\\.self\\.value", "mha.value"),
            ("attention\\.output\\.dense", "mha.output"),
            ("attention\\.output\\.LayerNorm", "norm_1"),
            ("intermediate\\.dense", "pwff.linear_inner"),
            ("(layers\\.[0-9]+)\\.output\\.dense", "$1.pwff.linear_outer"),
            ("(layers\\.[0-9]+)\\.output\\.LayerNorm", "$1.norm_2"),
            ("embeddings\\.LayerNorm", "embeddings.layer_norm"),
        ])
        .map_err(|e| anyhow!("remap: {e}"))?;
        let mut store = SafetensorsStore::from_file(dir.join("model.safetensors"))
            .with_from_adapter(PyTorchToBurnAdapter)
            .remap(remap);
        let result = model
            .load_from(&mut store)
            .map_err(|e| anyhow!("load weights: {e}"))?;
        // A missing tensor would leave random init in place and still "work";
        // fail loudly instead. (`embeddings.position_ids` and the pooler are
        // expected to be unused.)
        if !result.missing.is_empty() || !result.errors.is_empty() {
            return Err(anyhow!(
                "weight load incomplete: missing={:?} errors={:?}",
                result.missing,
                result.errors
            ));
        }
        Ok(Self { model, device })
    }
}

impl Engine for BurnEngine {
    fn forward(&mut self, b: &Batch) -> Result<Vec<f32>> {
        let shape = [b.batch, b.seq];
        let t = |v: &[i64]| {
            Tensor::<B, 2, Int>::from_data(TensorData::new(v.to_vec(), shape), &self.device)
        };
        let out = self.model.forward(
            t(&b.input_ids),
            t(&b.token_type_ids),
            t(&b.attention_mask),
        );
        debug_assert_eq!(out.dims()[2], HIDDEN);
        out.into_data()
            .into_vec::<f32>()
            .map_err(|e| anyhow!("read output: {e:?}"))
    }
}
