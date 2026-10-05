//! ONNX Runtime engine: the same `ort` version and `load-dynamic` linking as
//! EngramDB's default build. Session options mirror the production embedding
//! path (fastembed): graph optimization level 3, ORT's default intra-op
//! thread count unless `ENGINE_THREADS` is set.

use crate::common::{Batch, Engine};
use anyhow::{Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use std::path::Path;

pub struct OrtEngine {
    session: Session,
    has_token_type_ids: bool,
}

impl OrtEngine {
    pub fn load(model: &Path, threads: Option<usize>) -> Result<Self> {
        let mut builder = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(n) = threads {
            builder = builder
                .with_intra_threads(n)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        let session = builder
            .commit_from_file(model)
            .with_context(|| format!("load {}", model.display()))?;
        let has_token_type_ids = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        Ok(Self {
            session,
            has_token_type_ids,
        })
    }
}

impl Engine for OrtEngine {
    fn forward(&mut self, b: &Batch) -> Result<Vec<f32>> {
        let shape = [b.batch, b.seq];
        let ids = TensorRef::from_array_view((shape, b.input_ids.as_slice()))?;
        let mask = TensorRef::from_array_view((shape, b.attention_mask.as_slice()))?;
        let types = TensorRef::from_array_view((shape, b.token_type_ids.as_slice()))?;
        let mut inputs: Vec<(std::borrow::Cow<str>, ort::session::SessionInputValue)> = vec![
            ("input_ids".into(), ids.into()),
            ("attention_mask".into(), mask.into()),
        ];
        if self.has_token_type_ids {
            inputs.push(("token_type_ids".into(), types.into()));
        }
        let outputs = self.session.run(inputs)?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    }
}
