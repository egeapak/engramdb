//! Provider implementations that delegate model work to the daemon.
//!
//! These satisfy the same trait seams as the in-process providers
//! (`EmbeddingProvider`, `NliProvider`, `Reranker`), so the retrieval engine,
//! create/ingest path, and query path call them without knowing the model
//! actually runs in another process. An MCP wired with these loads no models.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::client::DaemonHandle;
use super::protocol::{DaemonOp, DaemonRequest, DaemonResponse};
use crate::embeddings::EmbeddingProvider;
use crate::nli::{NliProvider, NliResult};
use crate::ops::EngineProviders;
use crate::retrieval::reranker::{RerankScore, Reranker};
use crate::types::{EmbeddingBackend, EngramConfig};

/// Shared per-(store, backend) routing state for the remote providers.
struct RemoteCtx {
    handle: Arc<DaemonHandle>,
    dir: String,
    backend: Option<EmbeddingBackend>,
}

impl RemoteCtx {
    async fn send(&self, op: DaemonOp) -> Result<DaemonResponse> {
        self.handle
            .request(DaemonRequest {
                dir: self.dir.clone(),
                backend: self.backend,
                op,
            })
            .await
    }
}

/// Embedding provider that forwards inference to the daemon.
pub struct RemoteEmbeddingProvider {
    ctx: Arc<RemoteCtx>,
    dimensions: usize,
    max_tokens: usize,
}

#[async_trait]
impl EmbeddingProvider for RemoteEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed_batch(&[text]).await?;
        v.pop()
            .ok_or_else(|| anyhow::anyhow!("daemon returned no embedding"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let texts = texts.iter().map(|s| s.to_string()).collect();
        match self.ctx.send(DaemonOp::Embed { texts }).await? {
            DaemonResponse::Embedded { vectors } => Ok(vectors),
            DaemonResponse::Error { message } => Err(anyhow::anyhow!(message)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        }
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn max_tokens(&self) -> usize {
        self.max_tokens
    }
}

/// NLI provider that forwards classification to the daemon.
pub struct RemoteNliProvider {
    ctx: Arc<RemoteCtx>,
}

#[async_trait]
impl NliProvider for RemoteNliProvider {
    async fn classify(&self, premise: &str, hypothesis: &str) -> Result<NliResult> {
        let mut r = self.classify_batch(&[(premise, hypothesis)]).await?;
        r.pop()
            .ok_or_else(|| anyhow::anyhow!("daemon returned no NLI result"))
    }

    async fn classify_batch(&self, pairs: &[(&str, &str)]) -> Result<Vec<NliResult>> {
        let pairs = pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        match self.ctx.send(DaemonOp::Classify { pairs }).await? {
            DaemonResponse::Classified { results } => Ok(results
                .into_iter()
                .map(|w| NliResult::from_probs(w.entailment, w.neutral, w.contradiction))
                .collect()),
            DaemonResponse::Error { message } => Err(anyhow::anyhow!(message)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        }
    }
}

/// Reranker that forwards cross-encoder scoring to the daemon.
pub struct RemoteReranker {
    ctx: Arc<RemoteCtx>,
}

#[async_trait]
impl Reranker for RemoteReranker {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankScore>> {
        let op = DaemonOp::Rerank {
            query: query.to_string(),
            documents: documents.to_vec(),
        };
        match self.ctx.send(op).await? {
            DaemonResponse::Reranked { scores } => Ok(scores
                .into_iter()
                .map(|(index, score)| RerankScore { index, score })
                .collect()),
            DaemonResponse::Error { message } => Err(anyhow::anyhow!(message)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        }
    }
}

/// Build the [`EngineProviders`] bundle backed by the daemon for the store at
/// `dir` under `config`.
///
/// One round-trip fetches the embedding model's dimensionality and token
/// limit (needed synchronously for chunking and vector-store schema
/// agreement) and doubles as the trigger that loads the model in the daemon.
/// Returns `None` if that round-trip fails, so the caller falls back to
/// in-process providers. NLI and reranker are wired only when enabled in
/// `config`, mirroring `resolve_engine_providers`.
pub async fn remote_providers(
    handle: Arc<DaemonHandle>,
    dir: String,
    backend: Option<EmbeddingBackend>,
    config: &EngramConfig,
) -> Option<EngineProviders> {
    let ctx = Arc::new(RemoteCtx {
        handle,
        dir,
        backend,
    });

    let (dimensions, max_tokens) = match ctx.send(DaemonOp::Meta).await {
        Ok(DaemonResponse::Meta {
            dimensions,
            max_tokens,
        }) => (dimensions, max_tokens),
        Ok(DaemonResponse::Error { message }) => {
            tracing::warn!("daemon has no embedding model ({message}); using in-process models");
            return None;
        }
        Ok(other) => {
            tracing::warn!("unexpected daemon meta response: {other:?}; using in-process models");
            return None;
        }
        Err(e) => {
            tracing::warn!("daemon meta request failed ({e}); using in-process models");
            return None;
        }
    };

    let embedding = Some(Arc::new(RemoteEmbeddingProvider {
        ctx: Arc::clone(&ctx),
        dimensions,
        max_tokens,
    }) as Arc<dyn EmbeddingProvider>);

    let nli = config.nli.enabled.then(|| {
        Arc::new(RemoteNliProvider {
            ctx: Arc::clone(&ctx),
        }) as Arc<dyn NliProvider>
    });

    let reranker = config.rerank.enabled.then(|| {
        Arc::new(RemoteReranker {
            ctx: Arc::clone(&ctx),
        }) as Arc<dyn Reranker>
    });

    Some(EngineProviders {
        embedding,
        nli,
        reranker,
    })
}
