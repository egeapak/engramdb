//! Cross-encoder reranking abstraction.
//!
//! The retrieval engine refines its initial bi-encoder ranking with an
//! optional cross-encoder. Hiding the concrete `fastembed::TextRerank` behind
//! a trait lets the model live either in-process ([`LocalReranker`]) or in the
//! shared embedding daemon (`crate::daemon::remote::RemoteReranker`), so an MCP
//! process that delegates to the daemon never loads the reranker model itself.

use anyhow::Result;
use async_trait::async_trait;
use fastembed::TextRerank;
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
