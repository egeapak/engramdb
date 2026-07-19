//! Cross-encoder reranking abstraction and its `fastembed` loader.
//!
//! The retrieval engine refines its initial bi-encoder ranking with an optional
//! cross-encoder. Hiding the concrete `fastembed::TextRerank` behind a trait
//! lets the model live either in-process ([`LocalReranker`]) or in the shared
//! embedding daemon (the core's `daemon::remote::RemoteReranker`), so an MCP
//! process that delegates to the daemon never loads the reranker model itself.
//!
//! This lives in `engram-models` next to its embedding / NLI / T5 siblings; the
//! core re-exports it as `engramdb::retrieval::reranker` so callers keep their
//! historical import path.

use anyhow::Result;
use async_trait::async_trait;
#[cfg(feature = "onnxruntime")]
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
#[cfg(feature = "onnxruntime")]
use std::sync::{Arc, Mutex};

/// A cross-encoder score for one input document.
#[derive(Debug, Clone, Copy)]
pub struct RerankScore {
    /// Index of the document in the slice passed to [`Reranker::rerank`].
    pub index: usize,
    /// Raw (unbounded) cross-encoder logit. Callers normalize as needed.
    pub score: f32,
}

/// Jointly scores a query against candidate documents.
#[async_trait]
pub trait Reranker: Send + Sync {
    /// Score every `document` against `query`. The returned scores carry the
    /// original document index and may be in any order. Implementations must
    /// not reorder or drop the caller's candidate list themselves.
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>>;
}

/// In-process reranker backed by a `fastembed` cross-encoder.
///
/// `TextRerank::rerank` needs `&mut self` and is CPU-bound, so it is wrapped in
/// an `Arc<Mutex<_>>` and driven on a blocking thread.
#[cfg(feature = "onnxruntime")]
pub struct LocalReranker {
    inner: Arc<Mutex<TextRerank>>,
}

#[cfg(feature = "onnxruntime")]
impl LocalReranker {
    /// Wrap an already-loaded cross-encoder as a shared trait object.
    pub fn shared(inner: Arc<Mutex<TextRerank>>) -> Arc<dyn Reranker> {
        Arc::new(Self { inner })
    }

    /// Load the cross-encoder named by `model_name` and return it as a shared
    /// trait object. Mirrors the embedding loader's cache-dir + execution-
    /// provider wiring: models cache under [`engram_storage::paths::model_cache_dir`]
    /// and run on the ambient [`engram_onnx::execution_providers`]. A failed
    /// cache-dir lookup is an error, exactly like the embedding/NLI/T5
    /// loaders — falling back to a cwd-relative path would re-download the
    /// ~1 GB model into whatever project the process runs in, violating the
    /// unified-model-cache invariant.
    pub fn load(model_name: &str) -> Result<Arc<dyn Reranker>> {
        let cache_dir =
            engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

        let model = resolve_reranker_model(model_name);
        let mut options = RerankInitOptions::new(model)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let eps = engram_onnx::execution_providers();
        if !eps.is_empty() {
            options = options.with_execution_providers(eps);
        }

        let reranker = TextRerank::try_new(options).map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(Self::shared(Arc::new(Mutex::new(reranker))))
    }
}

#[cfg(feature = "onnxruntime")]
#[async_trait]
impl Reranker for LocalReranker {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>> {
        let inner = Arc::clone(&self.inner);
        let query = query.to_string();
        let documents = documents.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire reranker lock: {}", e))?;
            let doc_refs: Vec<&String> = documents.iter().collect();
            let results = guard
                .rerank(&query, doc_refs, false, None)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {}", e))?;
            Ok(results
                .into_iter()
                .map(|r| RerankScore {
                    index: r.index,
                    score: r.score,
                })
                .collect())
        })
        .await
        .map_err(|e| anyhow::anyhow!("Rerank task panicked: {}", e))?
    }
}

/// Map a reranker model name string to a fastembed `RerankerModel` enum variant.
///
/// The recognized default name is `bge-reranker-base`; anything else
/// unrecognized falls back to it WITH a warning — a silent fallback let a
/// typo (`bge-reranker-v2m3`) rerank with a different model than the user
/// believes they configured.
#[cfg(feature = "onnxruntime")]
fn resolve_reranker_model(name: &str) -> RerankerModel {
    match name {
        "bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
        "jina-reranker-v1-turbo-en" => RerankerModel::JINARerankerV1TurboEn,
        "jina-reranker-v2-base-multilingual" => RerankerModel::JINARerankerV2BaseMultiligual,
        "bge-reranker-base" => RerankerModel::BGERerankerBase,
        other => {
            tracing::warn!(
                "unknown rerank.model '{}'; falling back to bge-reranker-base \
                 (known: bge-reranker-base, bge-reranker-v2-m3, jina-reranker-v1-turbo-en, \
                 jina-reranker-v2-base-multilingual)",
                other
            );
            RerankerModel::BGERerankerBase
        }
    }
}

/// Pure-Rust cross-encoder reranker backed by tract (fp32 BGE), for the
/// Intel-Mac / `--features tract` build where `fastembed` is unavailable.
///
/// Only the default `bge-reranker-base` is supported: its fp32 ONNX export
/// (`Xenova/bge-reranker-base`, `onnx/model.onnx`) loads cleanly under tract
/// (verified: relevant pairs score high, irrelevant low). The int8 exports and
/// the other reranker models are not offered on the tract path. The model is a
/// large fp32 download (~1.1 GB) and runs ~3× slower than ONNX, so reranking
/// stays **off by default**; this only builds when `[rerank].enabled = true` on
/// a tract build.
#[cfg(feature = "tract")]
mod tract_reranker {
    use super::{RerankScore, Reranker};
    use anyhow::{Context, Result};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokenizers::Tokenizer;
    use tract_onnx::prelude::*;

    type RunnableTractModel = Arc<RunnableModel<TypedFact, Box<dyn TypedOp>>>;

    /// fp32 BGE reranker files (BERT-style: `input_ids` + `attention_mask`,
    /// single relevance logit output). 512-token pairs.
    const BGE_REPO: &str = "Xenova/bge-reranker-base";
    const BGE_MODEL_FILE: &str = "onnx/model.onnx";
    const BGE_TOKENIZER_FILE: &str = "tokenizer.json";
    const MAX_TOKENS: usize = 512;

    pub struct TractReranker {
        model: RunnableTractModel,
        tokenizer: Arc<Tokenizer>,
        /// Per graph input (in run order): true = attention mask, false = ids.
        input_is_mask: Vec<bool>,
    }

    impl TractReranker {
        /// Load the tract reranker for `model_name`. Only `bge-reranker-base`
        /// (the default) is supported on tract; anything else errors so the
        /// caller can continue without a reranker.
        pub fn load(model_name: &str) -> Result<Arc<dyn Reranker>> {
            if !matches!(model_name, "bge-reranker-base" | "") {
                anyhow::bail!(
                    "tract reranker only supports 'bge-reranker-base' (got '{model_name}'); \
                     reranking disabled on this pure-Rust build"
                );
            }

            let cache_dir =
                engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;
            if engram_storage::paths::offline_enabled()
                && !engram_storage::paths::hf_repo_cached(BGE_REPO)
            {
                anyhow::bail!("offline mode and tract reranker '{BGE_REPO}' is not cached");
            }

            let api = hf_hub::api::sync::ApiBuilder::new()
                .with_cache_dir(cache_dir)
                .build()
                .context("init HuggingFace API")?;
            let repo = api.model(BGE_REPO.to_string());
            let model_path = repo.get(BGE_MODEL_FILE).context("fetch BGE model")?;
            let tokenizer_path = repo
                .get(BGE_TOKENIZER_FILE)
                .context("fetch BGE tokenizer")?;

            let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
                .map_err(|e| anyhow::anyhow!("load tokenizer: {e}"))?;
            // Don't trust the repo tokenizer.json's baked-in truncation or
            // padding (the sibling MiniLM export ships a 128-token cap —
            // see `embeddings::tract`). Truncate pairs at this model's real
            // window; `score_blocking` pads to the fixed shape itself.
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: MAX_TOKENS,
                    ..Default::default()
                }))
                .map_err(|e| anyhow::anyhow!("set tokenizer truncation: {e}"))?;
            tokenizer.with_padding(None);

            let mut infer = tract_onnx::onnx()
                .model_for_path(&model_path)
                .context("tract model_for_path (BGE reranker)")?;
            let input_names: Vec<String> = infer
                .input_outlets()?
                .iter()
                .map(|o| infer.node(o.node).name.clone())
                .collect();
            let input_is_mask: Vec<bool> = input_names
                .iter()
                .map(|n| n.to_lowercase().contains("mask"))
                .collect();
            for i in 0..input_names.len() {
                infer = infer.with_input_fact(
                    i,
                    InferenceFact::dt_shape(i64::datum_type(), tvec!(1, MAX_TOKENS)),
                )?;
            }
            let model = infer.into_optimized()?.into_runnable()?;

            Ok(Arc::new(Self {
                model,
                tokenizer: Arc::new(tokenizer),
                input_is_mask,
            }))
        }

        fn score_blocking(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>> {
            let seq = MAX_TOKENS;
            let mut out = Vec::with_capacity(documents.len());
            for (index, doc) in documents.iter().enumerate() {
                let enc = self
                    .tokenizer
                    .encode((query, doc.as_str()), true)
                    .map_err(|e| anyhow::anyhow!("tokenize pair: {e}"))?;
                let mut ids = vec![0i64; seq];
                let mut mask = vec![0i64; seq];
                let eids = enc.get_ids();
                let emask = enc.get_attention_mask();
                let n = eids.len().min(seq);
                for t in 0..n {
                    ids[t] = eids[t] as i64;
                    mask[t] = emask[t] as i64;
                }
                let inputs: TVec<TValue> = self
                    .input_is_mask
                    .iter()
                    .map(|&is_mask| {
                        let v = if is_mask { &mask } else { &ids };
                        let t: Tensor =
                            tract_ndarray::Array2::<i64>::from_shape_vec((1, seq), v.clone())
                                .expect("fixed [1, seq]")
                                .into();
                        t.into()
                    })
                    .collect();
                let result = self.model.run(inputs)?;
                let view = result[0].to_plain_array_view::<f32>()?;
                // Sequence-classification head → single relevance logit.
                let score = view.iter().copied().next().unwrap_or(f32::NEG_INFINITY);
                out.push(RerankScore { index, score });
            }
            Ok(out)
        }
    }

    #[async_trait]
    impl Reranker for TractReranker {
        async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>> {
            if documents.is_empty() {
                return Ok(Vec::new());
            }
            let this = Arc::new(Self {
                model: Arc::clone(&self.model),
                tokenizer: Arc::clone(&self.tokenizer),
                input_is_mask: self.input_is_mask.clone(),
            });
            let query = query.to_string();
            let documents = documents.to_vec();
            tokio::task::spawn_blocking(move || this.score_blocking(&query, &documents))
                .await
                .map_err(|e| anyhow::anyhow!("tract rerank task panicked: {e}"))?
        }
    }
}

#[cfg(feature = "tract")]
pub use tract_reranker::TractReranker;

#[cfg(all(test, feature = "tract"))]
mod tract_tests {
    use super::*;

    /// The tract fp32 BGE reranker must rank a relevant document above an
    /// irrelevant one. Self-skips if the ~1.1 GB model isn't cached (CI/offline).
    #[tokio::test]
    async fn tract_reranker_orders_relevant_first() {
        let Ok(reranker) = TractReranker::load("bge-reranker-base") else {
            eprintln!("Skipping: tract BGE reranker model not available");
            return;
        };
        let query = "what is the capital of France?";
        let docs = vec![
            "Bananas are a yellow tropical fruit.".to_string(),
            "Paris is the capital city of France.".to_string(),
        ];
        let scores = reranker.rerank(query, &docs).await.unwrap();
        assert_eq!(scores.len(), 2);
        let paris = scores.iter().find(|s| s.index == 1).unwrap().score;
        let banana = scores.iter().find(|s| s.index == 0).unwrap().score;
        assert!(
            paris > banana,
            "relevant doc must outscore irrelevant: paris={paris} banana={banana}"
        );
    }
}
