//! Operations layer for EngramDB.
//!
//! This module provides shared operation functions that both the CLI and MCP server
//! call into. Each operation takes typed input parameters and returns typed output
//! results — no CLI formatting or MCP serialization happens here.

pub mod challenge;
pub mod compress;
pub mod create;
pub mod delete;
pub mod doctor;
pub mod gc;
pub mod get;
pub mod list;
pub mod parsing;
pub mod projects;
pub mod query;
pub mod reindex;
pub mod resolve;
pub mod review;
pub mod stats;
pub mod update;

pub use challenge::{challenge_for_contradictions, challenge_memory, ChallengeResult};
pub use compress::{
    compress_apply, compress_candidates, CompressApplyResult, CompressCandidate,
    CompressCandidatesResult,
};
pub use create::{create_memory, validate_summary, CreateParams, CreateResult};
pub use delete::delete_memory;
pub use doctor::{
    doctor, doctor_environment, CheckStatus, DoctorResult, DoctorSection, EnvironmentCheck,
    EnvironmentDoctorResult,
};
pub use gc::{gc_memories, GcResult};
pub use get::get_memory;
pub use list::{list_memories, parse_sort_field, ListParams, SortField};
pub use parsing::{
    parse_decay_strategy, parse_detail_level, parse_memory_type, parse_status, parse_visibility,
    validate_score,
};
pub use query::query_memories;
pub use reindex::{reindex, ReindexResult};
pub use resolve::{resolve_memory, ResolveAction, ResolveParams, ResolveResult};
pub use review::{review_memories, ReviewParams};
pub use stats::{compute_stats, StoreStats};
pub use update::{update_memory, UpdateParams};

use crate::embeddings::{
    EmbeddingProvider, OnnxProvider, ONNX_MXBAI_EMBED_LARGE, ONNX_NOMIC_EMBED_TEXT,
};
#[cfg(feature = "ollama")]
use crate::embeddings::{OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT};
use crate::nli::{NliProvider, OnnxNliProvider};
use crate::retrieval::engine::RetrievalEngine;
use crate::retrieval::reranker::{LocalReranker, Reranker};
use crate::storage::MemoryStore;
use crate::types::{EmbeddingBackend, EngramConfig};
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Resolve the effective embedding backend from the layered override chain.
///
/// Priority: `cli_override` > `ENGRAMDB_EMBEDDING_BACKEND` env var > config value.
pub fn resolve_backend(
    config_backend: EmbeddingBackend,
    cli_override: Option<EmbeddingBackend>,
) -> EmbeddingBackend {
    if let Some(b) = cli_override {
        return b;
    }
    if let Ok(val) = std::env::var("ENGRAMDB_EMBEDDING_BACKEND") {
        if let Ok(b) = val.parse::<EmbeddingBackend>() {
            return b;
        }
        eprintln!(
            "Warning: invalid ENGRAMDB_EMBEDDING_BACKEND='{}', ignoring",
            val
        );
    }
    config_backend
}

/// Try to create an embedding provider for the given model name and backend.
fn resolve_provider(model: &str, backend: EmbeddingBackend) -> Option<Arc<dyn EmbeddingProvider>> {
    #[cfg(not(feature = "ollama"))]
    if backend == EmbeddingBackend::Ollama {
        eprintln!(
            "Warning: embedding backend 'ollama' selected but Ollama support is not compiled in"
        );
        return None;
    }

    match model {
        "onnx" | "all-minilm" => try_onnx_then_ollama(
            backend,
            || OnnxProvider::try_new().map(|p| Arc::new(p) as _),
            #[cfg(feature = "ollama")]
            || OllamaProvider::try_new(ALL_MINILM).map(|p| Arc::new(p) as _),
        ),
        "nomic-embed-text" => try_onnx_then_ollama(
            backend,
            || OnnxProvider::try_with_model(ONNX_NOMIC_EMBED_TEXT).map(|p| Arc::new(p) as _),
            #[cfg(feature = "ollama")]
            || OllamaProvider::try_new(NOMIC_EMBED_TEXT).map(|p| Arc::new(p) as _),
        ),
        "mxbai-embed-large" => try_onnx_then_ollama(
            backend,
            || OnnxProvider::try_with_model(ONNX_MXBAI_EMBED_LARGE).map(|p| Arc::new(p) as _),
            #[cfg(feature = "ollama")]
            || OllamaProvider::try_new(MXBAI_EMBED_LARGE).map(|p| Arc::new(p) as _),
        ),
        other => {
            eprintln!(
                "Warning: unknown embedding model '{}', embeddings disabled",
                other
            );
            None
        }
    }
}

/// Shared logic: try ONNX and/or Ollama based on the backend preference.
fn try_onnx_then_ollama(
    backend: EmbeddingBackend,
    try_onnx: impl FnOnce() -> Option<Arc<dyn EmbeddingProvider>>,
    #[cfg(feature = "ollama")] try_ollama: impl FnOnce() -> Option<Arc<dyn EmbeddingProvider>>,
) -> Option<Arc<dyn EmbeddingProvider>> {
    if backend != EmbeddingBackend::Ollama {
        if let Some(p) = try_onnx() {
            return Some(p);
        }
        if backend == EmbeddingBackend::Onnx {
            return None;
        }
    }
    #[cfg(feature = "ollama")]
    if backend != EmbeddingBackend::Onnx {
        return try_ollama();
    }
    None
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

/// The model-backed providers a [`RetrievalEngine`] is wired with.
///
/// Each field is an `Arc` over an in-memory model session (ONNX embedding
/// model, NLI classifier, cross-encoder reranker). Building these is the
/// expensive part of engine construction — loading the embedding model alone
/// is a ~240ms ONNX session init. The providers carry no per-store state, so
/// a single bundle is safely shared across every project and every tool call
/// for the lifetime of a process. The MCP server caches one bundle per
/// distinct config signature so models load once instead of once per call.
#[derive(Clone, Default)]
pub struct EngineProviders {
    pub embedding: Option<Arc<dyn EmbeddingProvider>>,
    pub nli: Option<Arc<dyn NliProvider>>,
    pub reranker: Option<Arc<dyn Reranker>>,
}

/// Load the model-backed providers selected by `config`.
///
/// This is the heavyweight step: it initializes ONNX Runtime sessions for the
/// embedding model (always attempted) and, when enabled, the NLI and reranker
/// models. It performs blocking model loading and is intended to be called at
/// most once per process per distinct config (the MCP server caches the
/// result; the CLI calls it once and exits).
pub fn resolve_engine_providers(
    config: &crate::types::EngramConfig,
    backend_override: Option<EmbeddingBackend>,
) -> EngineProviders {
    let mut providers = EngineProviders::default();

    let backend = resolve_backend(config.embeddings.backend, backend_override);
    if let Some(provider) = resolve_provider(config.embeddings.provider.as_str(), backend) {
        if provider.dimensions() != config.embeddings.dimensions {
            eprintln!(
                "Warning: provider dimensions ({}) != config dimensions ({})",
                provider.dimensions(),
                config.embeddings.dimensions
            );
        }
        providers.embedding = Some(provider);
    }

    if config.nli.enabled {
        match OnnxNliProvider::try_new(&config.nli.model) {
            Some(provider) => providers.nli = Some(Arc::new(provider)),
            None => {
                eprintln!("Warning: NLI contradiction detection enabled but model unavailable")
            }
        }
    }

    if config.rerank.enabled {
        let cache_dir = crate::storage::paths::model_cache_dir()
            .unwrap_or_else(|_| PathBuf::from(".cache/engramdb/models"));

        let model = resolve_reranker_model(&config.rerank.model);
        let options = RerankInitOptions::new(model)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);

        match TextRerank::try_new(options) {
            Ok(reranker) => {
                providers.reranker = Some(LocalReranker::shared(Arc::new(Mutex::new(reranker))))
            }
            Err(e) => eprintln!("Warning: reranker init failed, continuing without: {}", e),
        }
    }

    providers
}

/// Assemble a [`RetrievalEngine`] from a store, config, and pre-built
/// providers. This is the cheap part of engine construction — it just wires
/// already-loaded model sessions onto the per-store engine, doing no I/O or
/// model loading.
pub fn assemble_engine(
    store: MemoryStore,
    config: crate::types::EngramConfig,
    providers: EngineProviders,
) -> RetrievalEngine {
    let mut engine = RetrievalEngine::new(store, config);
    if let Some(p) = providers.embedding {
        engine = engine.with_embedding_provider(p);
    }
    if let Some(p) = providers.nli {
        engine = engine.with_nli_provider(p);
    }
    if let Some(r) = providers.reranker {
        engine = engine.with_reranker(r);
    }
    engine
}

/// Build a `RetrievalEngine` with optional embeddings and reranker from a store + config path.
///
/// This is the shared helper that CLI and MCP callers use so they don't duplicate
/// the provider wiring.  Returns an engine that always works for storage operations;
/// embeddings and reranking are attached on a best-effort basis.  Vector storage is
/// handled by the MemoryStore's integrated LanceDB.
///
/// This rebuilds the model providers on every call. Long-lived callers that
/// invoke this per request (the MCP server) should instead cache
/// [`resolve_engine_providers`] and use [`assemble_engine`] so the embedding
/// model isn't reloaded on every tool call.
pub async fn build_engine(
    store: MemoryStore,
    config_path: &std::path::Path,
    backend_override: Option<EmbeddingBackend>,
) -> RetrievalEngine {
    let config = crate::storage::config::load_config(config_path)
        .await
        .unwrap_or_default();
    let providers = resolve_engine_providers(&config, backend_override);
    assemble_engine(store, config, providers)
}

/// Signature of the provider-relevant config fields.
///
/// Two configs with the same key resolve to interchangeable model sessions, so
/// the bundle can be shared. The resolved embedding backend is folded in so a
/// CLI/env backend override doesn't collide with the config-default backend.
pub fn provider_cache_key(
    config: &EngramConfig,
    backend_override: Option<EmbeddingBackend>,
) -> String {
    let backend = resolve_backend(config.embeddings.backend, backend_override);
    format!(
        "{backend}|{}|{}|{}|{}|{}|{}",
        config.embeddings.provider,
        config.embeddings.dimensions,
        config.nli.enabled,
        config.nli.model,
        config.rerank.enabled,
        config.rerank.model,
    )
}

/// Process-wide cache of model-backed [`EngineProviders`], keyed by
/// [`provider_cache_key`].
///
/// Loading the ONNX embedding model is a ~240ms session init (NLI / reranker
/// add more). This cache makes the models load at most once per distinct
/// config for the life of the process. It backs both the in-process MCP
/// fallback path and the shared embedding daemon, so both share identical
/// load-once semantics. `providers` carry no per-store state, so one bundle is
/// reused across every project and call.
#[derive(Clone, Default)]
pub struct ProviderCache {
    inner: Arc<tokio::sync::Mutex<std::collections::HashMap<String, EngineProviders>>>,
}

impl ProviderCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve providers for `config`, building them at most once per signature.
    ///
    /// The blocking model load runs on a blocking thread; the async mutex is
    /// held across it so concurrent first callers (e.g. a tool call racing the
    /// startup warmup) collapse into a single load instead of each loading the
    /// model.
    pub async fn get(
        &self,
        config: &EngramConfig,
        backend_override: Option<EmbeddingBackend>,
    ) -> EngineProviders {
        let key = provider_cache_key(config, backend_override);
        let mut guard = self.inner.lock().await;
        if let Some(p) = guard.get(&key) {
            return p.clone();
        }
        let cfg = config.clone();
        let providers =
            tokio::task::spawn_blocking(move || resolve_engine_providers(&cfg, backend_override))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("engine provider init task panicked: {e}");
                    EngineProviders::default()
                });
        guard.insert(key, providers.clone());
        providers
    }
}
