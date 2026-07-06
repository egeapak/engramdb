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
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
use std::path::PathBuf;
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
pub struct LocalReranker {
    inner: Arc<Mutex<TextRerank>>,
}

impl LocalReranker {
    /// Wrap an already-loaded cross-encoder as a shared trait object.
    pub fn shared(inner: Arc<Mutex<TextRerank>>) -> Arc<dyn Reranker> {
        Arc::new(Self { inner })
    }

    /// Load the cross-encoder named by `model_name` and return it as a shared
    /// trait object. Mirrors the embedding loader's cache-dir + execution-
    /// provider wiring: models cache under [`engram_storage::paths::model_cache_dir`]
    /// and run on the ambient [`engram_onnx::execution_providers`]. Fails only
    /// if `TextRerank::try_new` fails (e.g. model download/load error); the
    /// cache-dir lookup falls back to a relative path rather than erroring.
    pub fn load(model_name: &str) -> Result<Arc<dyn Reranker>> {
        let cache_dir = engram_storage::paths::model_cache_dir()
            .unwrap_or_else(|_| PathBuf::from(".cache/engramdb/models"));

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
fn resolve_reranker_model(name: &str) -> RerankerModel {
    match name {
        "bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
        "jina-reranker-v1-turbo-en" => RerankerModel::JINARerankerV1TurboEn,
        "jina-reranker-v2-base-multilingual" => RerankerModel::JINARerankerV2BaseMultiligual,
        _ => RerankerModel::BGERerankerBase, // default
    }
}
