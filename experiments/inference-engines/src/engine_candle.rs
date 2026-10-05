//! Candle engine: `candle-transformers`' stock `BertModel`, fp32 weights from
//! `sentence-transformers/all-MiniLM-L12-v2` (`model.safetensors`), pure-Rust
//! CPU backend (no MKL / Accelerate).

use crate::common::{Batch, Engine};
use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use std::path::Path;

pub struct CandleEngine {
    model: BertModel,
    device: Device,
}

impl CandleEngine {
    pub fn load(dir: &Path) -> Result<Self> {
        let device = Device::Cpu;
        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(dir.join("config.json")).context("config.json")?,
        )?;
        // SAFETY: the file is not modified while mapped.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[dir.join("model.safetensors")],
                DType::F32,
                &device,
            )?
        };
        let model = BertModel::load(vb, &config)?;
        Ok(Self { model, device })
    }
}

impl Engine for CandleEngine {
    fn forward(&mut self, b: &Batch) -> Result<Vec<f32>> {
        let shape = (b.batch, b.seq);
        let ids = Tensor::from_slice(&b.input_ids, shape, &self.device)?;
        let types = Tensor::from_slice(&b.token_type_ids, shape, &self.device)?;
        let mask = Tensor::from_slice(&b.attention_mask, shape, &self.device)?;
        let out = self.model.forward(&ids, &types, Some(&mask))?;
        Ok(out.flatten_all()?.to_vec1::<f32>()?)
    }
}
