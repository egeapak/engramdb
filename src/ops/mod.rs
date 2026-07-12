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
pub mod maintenance;
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
    compress_apply, compress_candidates, CompressApplyParams, CompressApplyResult,
    CompressCandidate, CompressCandidatesResult,
};
pub use create::{create_memory, validate_summary, CreateParams, CreateResult};
pub use delete::delete_memory;
pub use doctor::{
    doctor, doctor_environment, validate_models, CheckStatus, DoctorResult, DoctorSection,
    EnvironmentCheck, EnvironmentDoctorResult,
};
pub use gc::{execute_gc_plan, gc_memories, plan_gc, GcCandidate, GcMaintenance, GcPlan, GcResult};
pub use get::get_memory;
pub use list::{list_memories, parse_sort_field, ListParams, SortField};
pub use maintenance::{auto_maintain, maintenance_status, MaintenanceReport, MaintenanceStatus};
pub use parsing::{
    parse_decay_strategy, parse_detail_level, parse_detail_level_or_default, parse_memory_type,
    parse_status, parse_type_filter, parse_visibility, validate_score,
};
pub use query::{merge_scored_memories, query_memories, query_memories_with_global};
pub use reindex::{reindex, ReindexResult};
pub use resolve::{resolve_memory, ResolveAction, ResolveParams, ResolveResult};
pub use review::{review_memories, ReviewParams};
pub use stats::{compute_stats, StoreStats};
pub use update::{update_memory, UpdateParams};

use crate::embeddings::EmbeddingProvider;
#[cfg(feature = "ollama")]
use crate::embeddings::{
    OllamaModelSpec, OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT,
};
#[cfg(feature = "onnxruntime")]
use crate::embeddings::{
    OnnxModelSpec, OnnxProvider, DEFAULT_ONNX_EMBEDDING, ONNX_MXBAI_EMBED_LARGE,
    ONNX_NOMIC_EMBED_TEXT,
};
#[cfg(feature = "tract")]
use crate::embeddings::{TractEmbeddingProvider, TractModelSpec, TRACT_ALL_MINILM};
use crate::nli::NliProvider;
#[cfg(feature = "onnxruntime")]
use crate::nli::OnnxNliProvider;
use crate::retrieval::engine::RetrievalEngine;
#[cfg(feature = "onnxruntime")]
use crate::retrieval::reranker::LocalReranker;
use crate::retrieval::reranker::Reranker;
use crate::storage::{embedding_status, EmbeddingFingerprint, EmbeddingModelStatus, MemoryStore};
use crate::types::{EmbeddingBackend, EngramConfig};
use std::sync::Arc;

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

/// The model specs a configured `provider` string maps to.
///
/// **Single source of truth** for the provider→model table. Both
/// [`expected_embedding_fingerprint`] (cheap, no model load) and
/// [`resolve_provider`] (loads the model) derive from this one map, so the
/// fingerprint recorded for a store and the provider that actually runs can
/// never disagree on *which* model a config selects — adding a provider in
/// one place but not the other was a silent-vector-corruption footgun
/// flagged in branch review.
struct ProviderSpecs {
    #[cfg(feature = "onnxruntime")]
    onnx: OnnxModelSpec,
    /// The tract fp32 spec for this provider, when one exists. `None` for
    /// providers with no fp32 tract export (nomic / mxbai) — tract only ships
    /// the MiniLM fp32 model in the MVP.
    #[cfg(feature = "tract")]
    tract: Option<TractModelSpec>,
    #[cfg(feature = "ollama")]
    ollama: OllamaModelSpec,
}

/// Map a configured provider string to its ONNX / tract / Ollama model spec
/// (whichever backends are compiled in). `None` ⇒ unknown provider string
/// (embeddings disabled). The ONE place the provider→spec table lives.
fn provider_specs(provider: &str) -> Option<ProviderSpecs> {
    Some(match provider {
        "onnx" | "all-minilm" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: DEFAULT_ONNX_EMBEDDING,
            #[cfg(feature = "tract")]
            tract: Some(TRACT_ALL_MINILM),
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        "nomic-embed-text" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_NOMIC_EMBED_TEXT,
            #[cfg(feature = "tract")]
            tract: None,
            #[cfg(feature = "ollama")]
            ollama: NOMIC_EMBED_TEXT,
        },
        "mxbai-embed-large" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_MXBAI_EMBED_LARGE,
            #[cfg(feature = "tract")]
            tract: None,
            #[cfg(feature = "ollama")]
            ollama: MXBAI_EMBED_LARGE,
        },
        _ => return None,
    })
}

/// The embedding fingerprint `config` *would* produce, computed WITHOUT
/// loading the model — derived from the same [`provider_specs`] table and
/// backend preference [`resolve_provider`] uses. Used by `doctor` and the
/// open-time check (cheap); the enforcement guard uses the live provider's
/// `model_id()` instead. Returns `None` for an unknown provider string
/// (embeddings disabled).
// `return`s are load-bearing across feature combinations (later cfg blocks
// exist when ONNX Runtime is off) even though one combo makes the last one the
// tail expression.
#[allow(clippy::needless_return)]
pub fn expected_embedding_fingerprint(config: &EngramConfig) -> Option<EmbeddingFingerprint> {
    let backend = resolve_backend(config.embeddings.backend, None);
    let specs = provider_specs(config.embeddings.provider.as_str())?;

    // Explicit Ollama.
    #[cfg(feature = "ollama")]
    if backend == EmbeddingBackend::Ollama {
        return Some(EmbeddingFingerprint {
            model: format!("ollama/{}", specs.ollama.model_name),
            dimensions: specs.ollama.dimensions,
        });
    }

    // Explicit tract.
    #[cfg(feature = "tract")]
    if backend == EmbeddingBackend::Tract {
        let spec = specs.tract?;
        return Some(EmbeddingFingerprint {
            model: format!("tract/{}", spec.name),
            dimensions: spec.dimensions,
        });
    }

    // Auto / Onnx: prefer the ONNX identity when ONNX Runtime is compiled in
    // (Auto records the ONNX identity even if it would fall back to Ollama at
    // load, matching historical behavior).
    #[cfg(feature = "onnxruntime")]
    {
        let _ = backend;
        return Some(EmbeddingFingerprint {
            model: format!("onnx/{}", specs.onnx.name),
            dimensions: specs.onnx.dimensions,
        });
    }

    // No ONNX Runtime compiled in: Auto (the Intel-Mac default) resolves to
    // tract when available.
    #[cfg(all(not(feature = "onnxruntime"), feature = "tract"))]
    {
        let _ = backend;
        let spec = specs.tract?;
        return Some(EmbeddingFingerprint {
            model: format!("tract/{}", spec.name),
            dimensions: spec.dimensions,
        });
    }

    // Neither ONNX nor tract compiled in and backend isn't Ollama → embeddings
    // disabled.
    #[cfg(all(not(feature = "onnxruntime"), not(feature = "tract")))]
    {
        let _ = (backend, &specs);
        None
    }
}

/// Comparison of a store's stored embedding fingerprint vs the model in use.
pub struct EmbeddingModelReport {
    pub status: EmbeddingModelStatus,
    /// Actionable, user-facing message when not consistent (else `None`).
    pub warning: Option<String>,
}

/// Evaluate a store's stored embedding fingerprint against `current` (the
/// live provider's fingerprint, or `None` when embeddings are disabled).
/// One cheap manifest read; `status` is `Match` when embeddings are off.
pub async fn embedding_model_report(
    store: &MemoryStore,
    current: Option<EmbeddingFingerprint>,
) -> EmbeddingModelReport {
    let Some(current) = current else {
        return EmbeddingModelReport {
            status: EmbeddingModelStatus::Match,
            warning: None,
        };
    };
    let stored = store.embedding_fingerprint().await.ok().flatten();
    let status = embedding_status(stored.as_ref(), &current.model, current.dimensions);
    let warning = match &status {
        EmbeddingModelStatus::Match => None,
        EmbeddingModelStatus::Untracked { current } => Some(format!(
            "EngramDB: this store has no recorded embedding model (legacy store; \
             current model {current}). Memory search may use stale vectors — run \
             `engramdb reindex --embeddings-only` to re-embed and stamp it."
        )),
        EmbeddingModelStatus::Mismatch { stored, current } => Some(format!(
            "EngramDB: the embedding model changed (stored {stored}, current {current}). \
             Memory search is degraded until you run \
             `engramdb reindex --embeddings-only`."
        )),
        EmbeddingModelStatus::DimensionMismatch { stored, current } => Some(format!(
            "EngramDB: embedding dimensionality changed (stored {stored}, current \
             {current}). Run `engramdb reindex --embeddings-only` before using \
             memory search."
        )),
    };
    EmbeddingModelReport { status, warning }
}

/// Try to create an embedding provider for the given model name and backend.
///
/// Goes through [`provider_specs`] — the same table
/// [`expected_embedding_fingerprint`] uses — so the loaded model's identity
/// always matches the fingerprint recorded for the store.
// `return`s are load-bearing across feature combinations (fallback cfg blocks
// follow when a backend isn't compiled in).
#[allow(clippy::needless_return)]
fn resolve_provider(model: &str, backend: EmbeddingBackend) -> Option<Arc<dyn EmbeddingProvider>> {
    let Some(specs) = provider_specs(model) else {
        eprintln!(
            "Warning: unknown embedding model '{}', embeddings disabled",
            model
        );
        return None;
    };

    // Explicit Ollama backend.
    if backend == EmbeddingBackend::Ollama {
        #[cfg(feature = "ollama")]
        {
            return OllamaProvider::try_new(specs.ollama).map(|p| Arc::new(p) as _);
        }
        #[cfg(not(feature = "ollama"))]
        {
            eprintln!("Warning: embedding backend 'ollama' selected but Ollama support is not compiled in");
            return None;
        }
    }

    // Explicit tract backend (pure-Rust fp32).
    if backend == EmbeddingBackend::Tract {
        #[cfg(feature = "tract")]
        {
            return match specs.tract.and_then(TractEmbeddingProvider::try_with_model) {
                Some(p) => Some(Arc::new(p) as _),
                None => {
                    eprintln!(
                        "Warning: tract backend selected but no fp32 tract model available for '{}'",
                        model
                    );
                    None
                }
            };
        }
        #[cfg(not(feature = "tract"))]
        {
            eprintln!(
                "Warning: embedding backend 'tract' selected but tract support is not compiled in"
            );
            return None;
        }
    }

    // Auto / Onnx: try ONNX Runtime first when it is compiled in.
    #[cfg(feature = "onnxruntime")]
    {
        if let Some(p) = OnnxProvider::try_with_model(specs.onnx.clone())
            .map(|p| Arc::new(p) as Arc<dyn EmbeddingProvider>)
        {
            return Some(p);
        }
        if backend == EmbeddingBackend::Onnx {
            return None; // explicit Onnx: no fallback
        }
    }
    #[cfg(not(feature = "onnxruntime"))]
    if backend == EmbeddingBackend::Onnx {
        eprintln!("Warning: embedding backend 'onnx' selected but ONNX Runtime is not compiled in");
        return None;
    }

    // Auto fallback (ONNX unavailable / not compiled): tract, then Ollama. On a
    // pure-`tract` build this is how `Auto` — the Intel-Mac default — resolves.
    #[cfg(feature = "tract")]
    if backend != EmbeddingBackend::Onnx {
        if let Some(p) = specs
            .tract
            .and_then(TractEmbeddingProvider::try_with_model)
            .map(|p| Arc::new(p) as Arc<dyn EmbeddingProvider>)
        {
            return Some(p);
        }
    }
    #[cfg(feature = "ollama")]
    if backend != EmbeddingBackend::Onnx {
        return OllamaProvider::try_new(specs.ollama).map(|p| Arc::new(p) as _);
    }

    let _ = &specs;
    None
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
    /// Abstractive (T5) title generator, when `title.strategy = "t5"`.
    /// Keyword titling is in-process and cheap so it is never cached here;
    /// caching exists precisely because building T5 is an
    /// encoder+decoder ONNX init that otherwise ran on *every* `create`.
    pub title: Option<Arc<dyn crate::title::TitleGenerator>>,
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
    embedding_pool_size: usize,
) -> EngineProviders {
    let mut providers = EngineProviders::default();

    let backend = resolve_backend(config.embeddings.backend, backend_override);
    // Build up to `embedding_pool_size` independent sessions of the *same*
    // model and round-robin across them, so concurrent callers (the daemon
    // serving N agent sessions) don't all serialize behind one session's
    // mutex. A first-load failure leaves the pool empty → embeddings
    // disabled, exactly as a single-load failure behaved before; a later
    // session failing just yields a smaller pool (graceful degradation).
    let mut sessions = Vec::with_capacity(embedding_pool_size.max(1));
    for _ in 0..embedding_pool_size.max(1) {
        match resolve_provider(config.embeddings.provider.as_str(), backend) {
            Some(p) => sessions.push(p),
            None => break,
        }
    }
    if let Some(provider) = crate::embeddings::PooledEmbeddingProvider::build(sessions) {
        if provider.dimensions() != config.embeddings.dimensions {
            // Deliberately a warning, not a hard error: the Auto backend can
            // legitimately fall back to a provider with different dimensions
            // (e.g. ONNX unavailable → Ollama mxbai-embed-large) and graceful
            // degradation is the contract — operations must keep working.
            // Corruption is impossible regardless: the LanceDB index is sized
            // from the config value, `LanceIndex::upsert_chunks` rejects
            // wrong-width vectors at write time, `vector_search` rejects them
            // at read time, and the embedding-fingerprint check reports the
            // drift via `doctor`/open-time warnings.
            tracing::warn!(
                "Embedding provider '{}' produces {}-dimensional vectors but \
                 [embeddings].dimensions = {}. Embeddings will be rejected at write \
                 time until this is fixed — set [embeddings].dimensions = {} in \
                 config.toml, then run `engramdb reindex --embeddings-only`.",
                config.embeddings.provider,
                provider.dimensions(),
                config.embeddings.dimensions,
                provider.dimensions(),
            );
        }
        providers.embedding = Some(provider);
    }

    // NLI, cross-encoder reranker, and T5 titling are all ONNX-Runtime-only.
    // On a pure-`tract` build they are unavailable (deberta/BGE/T5 are quantized
    // ONNX exports that either need ORT or hit tract's static-shape wall), so
    // the whole block compiles out and those features stay off — matching the
    // MVP's "embeddings only on tract" contract.
    #[cfg(feature = "onnxruntime")]
    {
        if config.nli.enabled {
            match OnnxNliProvider::try_new(&config.nli.model) {
                Some(provider) => providers.nli = Some(Arc::new(provider)),
                None => {
                    eprintln!("Warning: NLI contradiction detection enabled but model unavailable")
                }
            }
        }

        if config.rerank.enabled {
            match LocalReranker::load(&config.rerank.model) {
                Ok(reranker) => providers.reranker = Some(reranker),
                Err(e) => eprintln!("Warning: reranker init failed, continuing without: {}", e),
            }
        }

        resolve_t5_title_provider(config, embedding_pool_size, &mut providers);
    }
    #[cfg(not(feature = "onnxruntime"))]
    {
        // NLI and T5 titling are ONNX-Runtime-only. The cross-encoder reranker,
        // however, has a pure-Rust tract path (fp32 BGE), so honor
        // `[rerank].enabled` when the `tract` backend is compiled in.
        if config.nli.enabled {
            eprintln!(
                "Warning: NLI contradiction detection is enabled but unavailable on a \
                 pure-tract build (no ONNX Runtime); continuing without it"
            );
        }
        #[cfg(feature = "tract")]
        if config.rerank.enabled {
            match crate::retrieval::reranker::TractReranker::load(&config.rerank.model) {
                Ok(reranker) => providers.reranker = Some(reranker),
                Err(e) => {
                    eprintln!(
                        "Warning: tract reranker init failed, continuing without: {}",
                        e
                    )
                }
            }
        }
        #[cfg(not(feature = "tract"))]
        if config.rerank.enabled {
            eprintln!("Warning: reranker is enabled but unavailable on this build");
        }
    }

    providers
}

/// Build the cached T5 title generator pool. Split out so the whole ORT-only
/// path (T5 is a quantized ONNX model) compiles out of a pure-`tract` build.
#[cfg(feature = "onnxruntime")]
fn resolve_t5_title_provider(
    config: &crate::types::EngramConfig,
    embedding_pool_size: usize,
    providers: &mut EngineProviders,
) {
    // Only T5 is worth caching: keyword titling is in-process and cheap, and
    // `none` builds nothing. Loading T5 here (an encoder+decoder ONNX init)
    // means the long-lived daemon / MCP server loads it once into the bundle
    // instead of rebuilding it on every single `create` — and, under
    // concurrency, pooling it cuts the dominant create-path tail latency.
    if config.title.strategy == crate::title::TitleStrategy::T5 {
        let cores = crate::types::config::available_cores();
        // The CLI passes `embedding_pool_size == 1` (one-shot, no
        // concurrency) — don't pay N× the heavy T5 load there. Long-lived
        // daemon / MCP (pool > 1) pool T5 too. Each session's intra_threads
        // is reduced so `pool × intra ≤ cores` (T5 sessions are direct ORT
        // sessions, unlike fastembed), capped at the Lever A sweet spot.
        let title_pool = if embedding_pool_size <= 1 {
            1
        } else {
            config.title.resolved_pool_size(cores)
        };
        let intra = (cores / title_pool.max(1))
            .min(crate::onnx_ep::intra_threads())
            .max(1);
        let mut generators: Vec<Arc<dyn crate::title::TitleGenerator>> =
            Vec::with_capacity(title_pool);
        for _ in 0..title_pool {
            match crate::title::t5::T5TitleGenerator::try_new_with_intra(intra) {
                Some(gen) => generators.push(Arc::new(gen)),
                None => break,
            }
        }
        match crate::title::PooledTitleGenerator::build(generators) {
            Some(t) => providers.title = Some(t),
            None => {
                eprintln!("Warning: title strategy 't5' configured but the T5 model is unavailable")
            }
        }
    }
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
    if let Some(t) = providers.title {
        engine = engine.with_title_provider(t);
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
    let config = crate::storage::config::load_config_or_default(config_path).await;
    // The CLI is one-shot (one op, then exit) with no concurrency, so a pool
    // would only pay N× the ~240ms model load for no throughput gain. Force
    // a single embedding session here regardless of config; pool auto-sizing
    // is for the long-lived daemon / MCP server.
    let providers = resolve_engine_providers(&config, backend_override, 1);
    assemble_engine(store, config, providers)
}

/// Build a `RetrievalEngine` with **no** model providers from a store + config path.
///
/// For callers that provably never exercise embeddings, NLI, reranking, or
/// title generation — e.g. the Claude Code hook handlers, whose queries carry
/// no query text (`query: None` → the engine's semantic step is skipped and
/// scoring is `scope_only`) and which never create memories. Unlike
/// [`build_engine`], this skips [`resolve_engine_providers`] entirely,
/// avoiding the ~240ms ONNX embedding session init (plus the T5
/// encoder+decoder init when `title.strategy = "t5"`) on every invocation.
pub async fn build_engine_without_providers(
    store: MemoryStore,
    config_path: &std::path::Path,
) -> RetrievalEngine {
    let config = crate::storage::config::load_config_or_default(config_path).await;
    assemble_engine(store, config, EngineProviders::default())
}

/// Signature of the provider-relevant config fields.
///
/// Two configs with the same key resolve to interchangeable model sessions, so
/// the bundle can be shared. The resolved embedding backend is folded in so a
/// CLI/env backend override doesn't collide with the config-default backend.
///
/// The four model-selecting config sections are destructured **exhaustively**
/// (no `..`): adding a field to any of them fails compilation here, forcing
/// an explicit decision about whether the new field affects which models get
/// loaded. This key drifted behind the config twice before (title.strategy,
/// title.pool_size — each a stale-bundle bug); the destructuring makes the
/// third drift impossible to miss.
pub fn provider_cache_key(
    config: &EngramConfig,
    backend_override: Option<EmbeddingBackend>,
    embedding_pool_size: usize,
) -> String {
    // Every field must be bound (or explicitly discarded with a reason).
    let crate::types::config::EmbeddingsConfig {
        backend: config_backend,
        provider,
        dimensions,
        // Chunking width, not a model identity: the same loaded session
        // serves any max_tokens (effective_chunk_tokens clamps per call).
        max_tokens: _,
        // Reindex *policy* — what to do on a model change, not which model.
        reindex_on_model_change: _,
        // The caller passes the RESOLVED pool size (auto `cores/2` applied),
        // which is what actually sizes the bundle.
        pool_size: _,
    } = &config.embeddings;
    let crate::types::config::NliConfig {
        enabled: nli_enabled,
        model: nli_model,
        // Inference-time thresholds, applied per call — not model identity.
        contradiction_threshold: _,
        max_comparisons: _,
        similarity_threshold: _,
    } = &config.nli;
    let crate::types::config::RerankConfig {
        enabled: rerank_enabled,
        model: rerank_model,
        // Query-time knobs on an already-loaded reranker.
        top_n: _,
        weight: _,
    } = &config.rerank;
    let crate::types::config::TitleConfig {
        strategy: title_strategy,
        // The T5 title pool is sized from this inside
        // `resolve_engine_providers`, so two configs with different title
        // pool sizes are NOT interchangeable bundles — omitting it would
        // serve a stale wrong-sized title pool after a config change.
        pool_size: title_pool_size,
    } = &config.title;

    let backend = resolve_backend(*config_backend, backend_override);
    format!(
        "{backend}|{provider}|{dimensions}|{embedding_pool_size}|{nli_enabled}|{nli_model}|{rerank_enabled}|{rerank_model}|{title_strategy:?}|{title_pool_size:?}",
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
///
/// A bundle that failed to resolve everything the config requested (e.g. the
/// embedding model lost a concurrent download race, or the model cache was
/// not yet staged) is cached only for [`FAILED_BUNDLE_RETRY_AFTER`]: the
/// heartbeat-kept-alive daemon and long-lived MCP fallback must recover once
/// the cause is fixed, not serve the dead bundle for the process lifetime —
/// the same "cached a `None` failure forever" class `DaemonCell` fixed for
/// daemon handles.
#[derive(Clone, Default)]
pub struct ProviderCache {
    inner: Arc<tokio::sync::Mutex<std::collections::HashMap<String, CachedProviders>>>,
}

/// How long a bundle with failed providers is served from cache before the
/// next caller re-attempts the model load. Long enough that a broken machine
/// isn't paying a failed load per request, short enough that fixing the model
/// cache heals live daemons/sessions promptly.
///
/// Known tradeoff: the cache's single mutex is held across the load (the
/// pre-existing single-flight design), so on a machine where a provider is
/// *permanently* unavailable every retry briefly stalls concurrent lookups —
/// for every key — behind the failed attempt. Offline-mode loads fail fast,
/// bounding the stall; a per-key in-flight guard would remove it entirely if
/// this ever shows up in practice.
const FAILED_BUNDLE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

struct CachedProviders {
    providers: EngineProviders,
    /// Whether every provider the config requested actually resolved.
    complete: bool,
    built_at: std::time::Instant,
}

/// True when every provider requested by `config` resolved: embeddings are
/// always attempted; NLI/reranker only count when enabled; a title generator
/// only counts under the T5 strategy.
fn providers_complete(config: &EngramConfig, providers: &EngineProviders) -> bool {
    providers.embedding.is_some()
        && (!config.nli.enabled || providers.nli.is_some())
        && (!config.rerank.enabled || providers.reranker.is_some())
        && (config.title.strategy != crate::title::TitleStrategy::T5 || providers.title.is_some())
}

impl ProviderCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct provider bundles (config signatures) resident.
    pub async fn loaded_count(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// Resolve providers for `config`, building them at most once per signature.
    ///
    /// The blocking model load runs on a blocking thread; the async mutex is
    /// held across it so concurrent first callers collapse into a single load
    /// instead of each loading the model. Note this serializes the *first*
    /// build of every distinct signature process-wide (a cold load for one
    /// config briefly blocks a cache lookup for another). That's acceptable
    /// here: a process almost always uses a single signature, and with the
    /// daemon enabled (the default) this in-process path is only the fallback.
    /// Cached lookups are not serialized beyond the brief map lock.
    pub async fn get(
        &self,
        config: &EngramConfig,
        backend_override: Option<EmbeddingBackend>,
    ) -> EngineProviders {
        // The cache backs the long-lived daemon / in-process MCP fallback —
        // both serve many concurrent callers — so honor the configured (or
        // auto `cores/2`) embedding pool size. The size is folded into the
        // cache key so a config change re-resolves instead of handing back a
        // wrong-sized pool.
        let pool_size = config
            .embeddings
            .resolved_pool_size(crate::types::config::available_cores());
        let key = provider_cache_key(config, backend_override, pool_size);
        let mut guard = self.inner.lock().await;
        if let Some(entry) = guard.get(&key) {
            if entry.complete || entry.built_at.elapsed() < FAILED_BUNDLE_RETRY_AFTER {
                return entry.providers.clone();
            }
            // Incomplete bundle past its retry window: fall through and
            // re-attempt the load (overwriting the cached failure).
        }
        let cfg = config.clone();
        let providers = tokio::task::spawn_blocking(move || {
            resolve_engine_providers(&cfg, backend_override, pool_size)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("engine provider init task panicked: {e}");
            EngineProviders::default()
        });
        let complete = providers_complete(config, &providers);
        if !complete {
            tracing::warn!(
                "provider bundle resolved incompletely (embedding: {}, nli: {}, rerank: {}, title: {}); \
                 will re-attempt after {}s",
                providers.embedding.is_some(),
                providers.nli.is_some(),
                providers.reranker.is_some(),
                providers.title.is_some(),
                FAILED_BUNDLE_RETRY_AFTER.as_secs(),
            );
        }
        guard.insert(
            key,
            CachedProviders {
                providers: providers.clone(),
                complete,
                built_at: std::time::Instant::now(),
            },
        );
        providers
    }

    /// Test seam: insert a pre-built bundle with an explicit completeness and
    /// age, so retry semantics are testable without loading models.
    #[cfg(test)]
    async fn insert_for_test(
        &self,
        key: String,
        providers: EngineProviders,
        complete: bool,
        built_at: std::time::Instant,
    ) {
        self.inner.lock().await.insert(
            key,
            CachedProviders {
                providers,
                complete,
                built_at,
            },
        );
    }
}

#[cfg(test)]
mod provider_cache_tests {
    use super::*;

    #[test]
    fn cache_key_is_deterministic_and_signature_sensitive() {
        let base = EngramConfig::default();
        let k = provider_cache_key(&base, None, 2);
        // Deterministic.
        assert_eq!(k, provider_cache_key(&base, None, 2));

        // Backend override is folded in.
        assert_ne!(
            k,
            provider_cache_key(&base, Some(EmbeddingBackend::Onnx), 2)
        );

        // Pool size is folded in: two pools of different sizes are NOT
        // interchangeable model bundles (handing back a wrong-sized pool
        // would silently under- or over-provision the daemon).
        assert_ne!(k, provider_cache_key(&base, None, 4));
        assert_eq!(k, provider_cache_key(&base, None, 2));

        // Each provider-relevant field changes the key.
        let mut c = base.clone();
        c.embeddings.provider = "mxbai-embed-large".to_string();
        assert_ne!(k, provider_cache_key(&c, None, 2));

        let mut c = base.clone();
        c.embeddings.dimensions += 1;
        assert_ne!(k, provider_cache_key(&c, None, 2));

        let mut c = base.clone();
        c.nli.enabled = !c.nli.enabled;
        assert_ne!(k, provider_cache_key(&c, None, 2));

        let mut c = base.clone();
        c.nli.model = "other-nli".to_string();
        assert_ne!(k, provider_cache_key(&c, None, 2));

        let mut c = base.clone();
        c.rerank.enabled = !c.rerank.enabled;
        assert_ne!(k, provider_cache_key(&c, None, 2));

        let mut c = base.clone();
        c.rerank.model = "other-reranker".to_string();
        assert_ne!(k, provider_cache_key(&c, None, 2));

        // Title strategy is folded in: T5 pulls a cached generator into the
        // bundle, so it is a distinct signature from keyword/none. (The
        // default is now T5, so flip to keyword to prove sensitivity.)
        assert_eq!(base.title.strategy, crate::title::TitleStrategy::T5);
        let mut c = base.clone();
        c.title.strategy = crate::title::TitleStrategy::Keyword;
        assert_ne!(k, provider_cache_key(&c, None, 2));

        // Title pool size is folded in: it sizes the pooled T5 generator
        // inside `resolve_engine_providers`, so changing `[title].pool_size`
        // must re-resolve instead of serving a stale wrong-sized pool.
        assert_eq!(base.title.pool_size, None);
        let mut c = base.clone();
        c.title.pool_size = Some(3);
        assert_ne!(k, provider_cache_key(&c, None, 2));

        // A daemon-only config change does NOT change the model signature.
        let mut c = base.clone();
        c.daemon.idle_timeout_secs += 1;
        assert_eq!(k, provider_cache_key(&c, None, 2));
    }

    #[tokio::test]
    async fn provider_cache_starts_empty() {
        let cache = ProviderCache::new();
        assert_eq!(cache.loaded_count().await, 0);
    }

    /// Deterministically fast-failing config: unknown embedding provider
    /// (resolve_provider returns None without touching any model), NLI and
    /// rerank disabled, keyword titles (nothing to load).
    fn failing_config() -> EngramConfig {
        let mut config = EngramConfig::default();
        config.embeddings.provider = "definitely-not-a-model".to_string();
        config.nli.enabled = false;
        config.rerank.enabled = false;
        config.title.strategy = crate::title::TitleStrategy::Keyword;
        config
    }

    struct MarkerEmbedding;

    #[async_trait::async_trait]
    impl crate::embeddings::EmbeddingProvider for MarkerEmbedding {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0; 4]).collect())
        }
        fn dimensions(&self) -> usize {
            4
        }
        fn max_tokens(&self) -> usize {
            16
        }
        fn model_id(&self) -> String {
            "test/marker".to_string()
        }
    }

    fn marker_bundle() -> EngineProviders {
        EngineProviders {
            embedding: Some(Arc::new(MarkerEmbedding)),
            ..EngineProviders::default()
        }
    }

    fn key_for(config: &EngramConfig) -> String {
        let pool = config
            .embeddings
            .resolved_pool_size(crate::types::config::available_cores());
        provider_cache_key(config, None, pool)
    }

    #[test]
    fn providers_complete_tracks_requested_providers() {
        let config = failing_config();
        // Only embeddings are requested: empty bundle is incomplete, a bundle
        // with an embedding provider is complete.
        assert!(!providers_complete(&config, &EngineProviders::default()));
        assert!(providers_complete(&config, &marker_bundle()));

        // Enabling NLI makes a missing NLI provider incomplete again.
        let mut config = failing_config();
        config.nli.enabled = true;
        assert!(!providers_complete(&config, &marker_bundle()));

        // T5 titles require a title generator.
        let mut config = failing_config();
        config.title.strategy = crate::title::TitleStrategy::T5;
        assert!(!providers_complete(&config, &marker_bundle()));
    }

    /// A failed (incomplete) bundle must not be cached forever: within the
    /// retry window it is served from cache, past the window the next get()
    /// re-attempts the load. A complete bundle never expires.
    #[tokio::test]
    async fn provider_cache_retries_failed_bundles_after_window() {
        let config = failing_config();
        let key = key_for(&config);
        // Instant is monotonic-since-boot; on a very fresh machine there may
        // be no representable instant this far in the past — skip then.
        let Some(stale) = std::time::Instant::now().checked_sub(FAILED_BUNDLE_RETRY_AFTER * 2)
        else {
            return;
        };

        // Fresh incomplete entry: served from cache (marker survives).
        let cache = ProviderCache::new();
        cache
            .insert_for_test(
                key.clone(),
                marker_bundle(),
                false,
                std::time::Instant::now(),
            )
            .await;
        let got = cache.get(&config, None).await;
        assert_eq!(
            got.embedding.map(|e| e.model_id()),
            Some("test/marker".to_string()),
            "incomplete bundle inside the retry window must be served from cache"
        );

        // Stale incomplete entry: get() re-resolves (the failing config
        // resolves to an empty bundle, so the marker disappears).
        let cache = ProviderCache::new();
        cache
            .insert_for_test(key.clone(), marker_bundle(), false, stale)
            .await;
        let got = cache.get(&config, None).await;
        assert!(
            got.embedding.is_none(),
            "incomplete bundle past the retry window must be re-resolved"
        );

        // Stale but complete entry: never re-resolved.
        let cache = ProviderCache::new();
        cache
            .insert_for_test(key.clone(), marker_bundle(), true, stale)
            .await;
        let got = cache.get(&config, None).await;
        assert_eq!(
            got.embedding.map(|e| e.model_id()),
            Some("test/marker".to_string()),
            "complete bundles must be cached for the process lifetime"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onnx_config(provider: &str) -> EngramConfig {
        let mut config = EngramConfig::default();
        provider.clone_into(&mut config.embeddings.provider);
        // Pin the backend so the expected fingerprint is deterministic
        // regardless of the `ollama` feature / backend auto-resolution.
        config.embeddings.backend = EmbeddingBackend::Onnx;
        config
    }

    #[test]
    fn provider_specs_table_resolves_known_and_rejects_unknown() {
        assert!(provider_specs("onnx").is_some());
        assert!(provider_specs("all-minilm").is_some());
        assert!(provider_specs("nomic-embed-text").is_some());
        assert!(provider_specs("mxbai-embed-large").is_some());
        assert!(provider_specs("definitely-not-a-model").is_none());
    }

    #[cfg(feature = "onnxruntime")]
    #[test]
    fn expected_fingerprint_matches_spec_for_every_provider_string() {
        for (name, spec) in [
            ("onnx", DEFAULT_ONNX_EMBEDDING),
            ("all-minilm", DEFAULT_ONNX_EMBEDDING),
            ("nomic-embed-text", ONNX_NOMIC_EMBED_TEXT),
            ("mxbai-embed-large", ONNX_MXBAI_EMBED_LARGE),
        ] {
            let fp = expected_embedding_fingerprint(&onnx_config(name))
                .unwrap_or_else(|| panic!("known provider {name} must resolve"));
            assert_eq!(fp.model, format!("onnx/{}", spec.name));
            assert_eq!(fp.dimensions, spec.dimensions);
        }
    }

    #[test]
    fn expected_fingerprint_is_none_for_unknown_provider() {
        assert!(expected_embedding_fingerprint(&onnx_config("nope")).is_none());
    }

    /// The hook handlers' construction path: no embedding/NLI/reranker/title
    /// provider may be wired, regardless of config (the default config has
    /// embeddings enabled and `title.strategy = "t5"`, which `build_engine`
    /// would resolve). This is what keeps hook invocations free of any ONNX
    /// session init.
    #[tokio::test]
    async fn build_engine_without_providers_wires_no_models() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &crate::storage::InMemoryRegistry::new())
            .await
            .unwrap();
        let config_path = temp_dir.path().join(".engramdb").join("config.toml");

        let engine = build_engine_without_providers(store, &config_path).await;

        assert!(!engine.embeddings_available());
        assert!(!engine.nli_available());
        assert!(engine.title_generator().is_none());
    }

    /// The actual safety property the `provider_specs` unification
    /// protects: the cheap, model-load-free `expected_embedding_fingerprint`
    /// must equal what the *live* provider reports via `model_id()`. If they
    /// ever diverge, model-change detection silently passes mismatched
    /// vectors (the footgun flagged by 3 review agents). Locks the
    /// fingerprint path and the resolve path to the same table.
    #[cfg(feature = "onnxruntime")]
    #[test]
    fn expected_fingerprint_matches_live_default_provider() {
        let provider = OnnxProvider::try_new().expect("default ONNX model available in test env");
        let fp = expected_embedding_fingerprint(&onnx_config("onnx")).unwrap();
        assert_eq!(fp.model, provider.model_id());
        assert_eq!(fp.dimensions, provider.dimensions());
    }
}
