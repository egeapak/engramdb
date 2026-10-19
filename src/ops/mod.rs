//! Operations layer for EngramDB.
//!
//! This module provides shared operation functions that both the CLI and MCP server
//! call into. Each operation takes typed input parameters and returns typed output
//! results — no CLI formatting or MCP serialization happens here.

pub mod challenge;
pub mod compress;
pub mod config;
pub mod create;
pub mod delete;
pub mod discover;
pub mod doctor;
pub mod gc;
pub mod get;
pub mod harvest;
pub mod harvest_index;
pub mod harvest_pin;
pub mod list;
pub mod maintenance;
pub mod parsing;
pub mod projects;
pub(crate) mod provider_cache;
pub mod query;
pub mod reindex;
pub mod repair;
pub mod resolve;
pub mod review;
pub mod stats;
pub mod task;
pub mod update;
pub mod verify;

pub use challenge::{
    challenge_for_contradictions, challenge_memory, route_contradiction, ChallengeResult,
    ConflictAction, NewMemoryMeta,
};
pub use compress::{
    compress_apply, compress_candidates, consolidate_cluster_apply, consolidation_pass,
    CompressApplyParams, CompressApplyResult, CompressCandidate, CompressCandidatesResult,
    ConsolidationCluster, ConsolidationReport,
};
pub use config::{top_tags, AgentConfigView, TagCount, DEFAULT_TOP_TAGS};
pub use create::{create_memory, validate_summary, CreateParams, CreateResult};
pub use delete::delete_memory;
// `discover` is deliberately NOT flat-re-exported, matching its sibling
// `projects` (the registry-management family it belongs to): callers use
// `ops::discover::…`. A flat re-export would need `DEFAULT_MAX_DEPTH` aliased
// to something unique — the only aliased re-export in this file, and a sign the
// name is too generic for the crate-wide op surface.
pub use doctor::{
    doctor, doctor_environment, doctor_epistemic, enrichment_gaps, validate_models, CheckStatus,
    DoctorResult, DoctorSection, EnrichmentGaps, EnvironmentCheck, EnvironmentDoctorResult,
    EpistemicDoctorResult, EpistemicFinding, EpistemicFindingKind,
};
pub use gc::{
    execute_gc_plan, gc_memories, plan_gc, GcCandidate, GcMaintenance, GcPlan, GcReason, GcResult,
};
pub use get::get_memory;
pub use harvest::{
    budget_digest, digest_session, parse_since, render_digest_markdown, select_sessions,
    session_scope, DigestParams, SelectParams, SelectedSession, SessionDigest, SessionScope,
    DEFAULT_DIGEST_BUDGET,
};
pub use harvest_index::{
    forget_session, index_pending, index_session, index_sessions, open_index_for_rebuild,
    reindex_from_copies, search as search_conversations, set_summary as set_conversation_summary,
    IndexAction, IndexReport,
};
pub use harvest_pin::{
    evidence_links, expired_evidence, link_memories, sessions_with_bytes, EvidenceLinks,
    ExpiredEvidence, LinkReport,
};
pub use list::{list_memories, parse_sort_field, ListParams, SortField};
pub use maintenance::{
    auto_compact, auto_maintain, auto_maintain_with_engine, maintenance_status,
    maintenance_would_run, MaintenanceReport, MaintenanceStatus,
};
pub use parsing::{
    parse_decay_strategy, parse_detail_level, parse_detail_level_or_default, parse_epistemic,
    parse_epistemic_filter, parse_generality, parse_memory_type, parse_situation, parse_status,
    parse_type_filter, parse_visibility, validate_score,
};
pub use provider_cache::{provider_cache_key, ProviderCache};
pub use query::{
    audience_allows, merge_scored_memories, query_memories, query_memories_with_extra_stores,
    query_memories_with_global, ExtraStoresResult,
};
pub use reindex::{reindex, reindex_dry_run, ReindexOptions, ReindexPlan, ReindexResult};
pub use resolve::{resolve_memory, ResolveAction, ResolveParams, ResolveResult};
pub use review::{count_recency_stale, review_memories, ReviewParams};
pub use stats::{compute_stats, StoreStats};
pub use task::{
    count_retrieval_sessions, promote_reconfirmed_memories, task_complete, task_current,
    PromotionReport, TaskCompleteResult, TaskCurrentResult,
};
pub use update::{update_memory, UpdateParams};
pub use verify::{verify_memory, VerifyResult};

use crate::embeddings::EmbeddingProvider;
#[cfg(feature = "ollama")]
use crate::embeddings::{
    OllamaModelSpec, OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT,
};
#[cfg(feature = "onnxruntime")]
use crate::embeddings::{
    OnnxModelSpec, OnnxProvider, DEFAULT_ONNX_EMBEDDING, ONNX_ALL_MINILM_L12,
    ONNX_ALL_MINILM_L12_Q, ONNX_ALL_MINILM_L12_U8, ONNX_ALL_MINILM_L6_U8, ONNX_ALL_MINILM_Q,
    ONNX_BGE_LARGE_EN_Q, ONNX_GTE_LARGE_EN_Q, ONNX_MXBAI_EMBED_LARGE, ONNX_MXBAI_EMBED_LARGE_Q,
    ONNX_NOMIC_EMBED_TEXT,
};
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

/// Close the validity windows of memories superseded by `new_id` (§2.4
/// writer 1): each referenced LIVE memory gets `invalidated_at = now`,
/// `superseded_by = new_id`. Missing and already-invalidated ids are skipped
/// with a log line, matching the existing tolerance in `compress_apply` —
/// supersession is best-effort metadata maintenance, never a reason to fail
/// the create/update that carried it.
pub(crate) async fn close_superseded_windows(
    store: &MemoryStore,
    supersedes: &[String],
    new_id: &str,
) {
    let now = chrono::Utc::now();
    // One batched read for the whole supersedes list. `supersedes` is
    // caller-supplied and unbounded (an agent can pass an arbitrarily long
    // list via `compress apply`), and each `get` is a full directory scan.
    let targets: Vec<&str> = supersedes
        .iter()
        .map(String::as_str)
        .filter(|id| *id != new_id)
        .collect();
    let loaded: std::collections::HashMap<String, crate::types::Memory> = store
        .get_batch(&targets)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    for old_id in supersedes {
        if old_id.as_str() == new_id {
            tracing::debug!(id = %new_id, "memory lists itself in supersedes; skipping");
            continue;
        }
        match loaded.get(old_id).ok_or(()) {
            Ok(old) if old.is_invalidated_at(now) => {
                tracing::debug!(
                    superseded = %old_id,
                    by = %new_id,
                    "supersedes target already invalidated; skipping window close"
                );
            }
            Ok(_) => {
                if let Err(e) = store
                    .invalidate_with(old_id, Some(new_id.to_string()), now)
                    .await
                {
                    tracing::warn!(
                        superseded = %old_id,
                        by = %new_id,
                        "failed to close superseded memory's validity window: {e}"
                    );
                }
            }
            Err(()) => {
                tracing::debug!(
                    superseded = %old_id,
                    by = %new_id,
                    "supersedes target not found; skipping window close"
                );
            }
        }
    }
}

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
    #[cfg(feature = "ollama")]
    ollama: OllamaModelSpec,
}

/// Map a configured provider string to its ONNX / Ollama model spec
/// (whichever backends are compiled in). `None` ⇒ unknown provider string
/// (embeddings disabled). The ONE place the provider→spec table lives.
fn provider_specs(provider: &str) -> Option<ProviderSpecs> {
    Some(match provider {
        // `all-minilm` tracks whatever the shipped default is (today L12);
        // `all-minilm-l6` / `-l12` pin a specific depth, so a store can stay on
        // the old model — and skip the reindex — across a default change.
        "onnx" | "all-minilm" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: DEFAULT_ONNX_EMBEDDING,
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        "all-minilm-l6" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_ALL_MINILM_L6_U8,
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        "all-minilm-l12" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_ALL_MINILM_L12_U8,
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        // The signed-int8 exports. Kept selectable so an existing store can
        // stay on its current vectors without a reindex, and so the
        // reproducibility finding stays reproducible — but they are NOT a
        // default: ONNX Runtime executes them non-reproducibly under CPU load
        // (onnxruntime#6004). See `docs/contributors/embedding-model-alternatives.md` (R6).
        "all-minilm-l12-int8" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_ALL_MINILM_L12_Q,
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        "all-minilm-l6-int8" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_ALL_MINILM_Q,
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        // fp32: bit-exact like uint8, but 4x the disk and ~1.4x the latency.
        "all-minilm-l12-fp32" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_ALL_MINILM_L12,
            #[cfg(feature = "ollama")]
            ollama: ALL_MINILM,
        },
        "nomic-embed-text" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_NOMIC_EMBED_TEXT,
            #[cfg(feature = "ollama")]
            ollama: NOMIC_EMBED_TEXT,
        },
        "mxbai-embed-large" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_MXBAI_EMBED_LARGE,
            #[cfg(feature = "ollama")]
            ollama: MXBAI_EMBED_LARGE,
        },
        // The quantized 1024-dim family. Selectable, never a default: each is
        // 4-13x the shipped model's 34 MB on disk and resident in the daemon,
        // for a width EngramDB's scoring does not currently need. Measured in
        // `docs/contributors/query-latency-profile.md`.
        "mxbai-embed-large-q" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_MXBAI_EMBED_LARGE_Q,
            #[cfg(feature = "ollama")]
            ollama: MXBAI_EMBED_LARGE,
        },
        "bge-large-en-q" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_BGE_LARGE_EN_Q,
            #[cfg(feature = "ollama")]
            ollama: MXBAI_EMBED_LARGE,
        },
        "gte-large-en-q" => ProviderSpecs {
            #[cfg(feature = "onnxruntime")]
            onnx: ONNX_GTE_LARGE_EN_Q,
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
    let composition = config.embeddings.composition_id();

    // Explicit Ollama.
    #[cfg(feature = "ollama")]
    if backend == EmbeddingBackend::Ollama {
        return Some(EmbeddingFingerprint {
            model: format!("ollama/{}", specs.ollama.model_name),
            dimensions: specs.ollama.dimensions,
            composition,
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
            composition,
        });
    }

    // No ONNX Runtime compiled in and backend isn't Ollama → embeddings
    // disabled.
    #[cfg(not(feature = "onnxruntime"))]
    {
        let _ = (backend, &specs, composition);
        None
    }
}

/// Read the embedding fingerprint stamped in a shared store's manifest.
///
/// `store_id` is a group id (`__g_…`) or the global store id
/// (`GLOBAL_PROJECT_ID`); the path is resolved accordingly. Returns `None` when
/// the store doesn't exist, has no manifest, or the manifest carries no
/// fingerprint (a legacy/unstamped store — only `reindex` stamps one). One cheap
/// manifest read, no store open (so it never creates the store).
pub async fn shared_store_fingerprint(store_id: &str) -> Option<EmbeddingFingerprint> {
    let store_dir = if crate::storage::paths::is_group_id(store_id) {
        crate::storage::paths::group_store_dir(store_id).ok()?
    } else {
        crate::storage::paths::global_store_dir().ok()?
    };
    let manifest_path = crate::storage::paths::project_dir(&store_dir).join("manifest.toml");
    crate::storage::manifest::load_manifest(&manifest_path)
        .await
        .ok()?
        .embedding
}

/// Decide whether a cross-store fan-in warrants post-merge rerank
/// **equalization** (P2, multi-project memories).
///
/// Returns `true` iff the project resolves an embedding model *and* at least one
/// of `shared_store_ids` (subscribed group ids and/or the global store id) has a
/// stamped fingerprint that differs from the project's in **any** field — model,
/// dimensions, *or* composition. A composition difference (e.g. one store
/// embedded `"{summary} {content}"` and another added title/tag signal) skews
/// cross-store ranking exactly like a model change does — the same condition
/// `embedding_status` reports as `CompositionMismatch` — so it must trigger
/// equalization too. When every shared store either matches or is unstamped, this
/// returns `false` (the same-fingerprint fast path: no extra cross-encoder pass).
/// Conservative by design: an *unstamped* store does not trigger equalization (we
/// can't prove drift; the `doctor` group check nudges a reindex instead), and a
/// keyword-only project (no embedding model) never needs it because keyword
/// scores are model-independent and already comparable.
pub async fn cross_store_equalization_needed(
    config: &EngramConfig,
    shared_store_ids: &[String],
) -> bool {
    let Some(project_fp) = expected_embedding_fingerprint(config) else {
        return false;
    };
    for id in shared_store_ids {
        if let Some(store_fp) = shared_store_fingerprint(id).await {
            // Whole-fingerprint compare (`EmbeddingFingerprint: PartialEq`) so a
            // composition-only drift is caught alongside model/dimension drift.
            if project_fp != store_fp {
                return true;
            }
        }
    }
    false
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
    let status = embedding_status(
        stored.as_ref(),
        &current.model,
        current.dimensions,
        current.composition.as_deref(),
    );
    // Untracked warns about vectors of unknown model vintage — but a store
    // with NO vectors has nothing stale to warn about. A brand-new project
    // is unstamped until its first embed (which stamps it — see
    // `embed_memory_with`), and without this gate every query on it claimed
    // "legacy store" and prescribed a pointless `reindex --embeddings-only`.
    // On a count failure keep the warning: false alarm beats false silence.
    if matches!(status, EmbeddingModelStatus::Untracked { .. })
        && matches!(store.has_any_chunks().await, Ok(false))
    {
        return EmbeddingModelReport {
            status: EmbeddingModelStatus::Match,
            warning: None,
        };
    }
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
        EmbeddingModelStatus::CompositionMismatch { stored, current } => {
            let stored = stored.as_deref().unwrap_or("legacy");
            let current = current.as_deref().unwrap_or("legacy");
            Some(format!(
                "EngramDB: the embedding text composition changed (stored {stored}, \
                 current {current} — e.g. the metadata-vector default). Vectors \
                 embedded before the change are ranked inconsistently with new \
                 ones — run `engramdb reindex --embeddings-only` to re-embed."
            ))
        }
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
///
/// `intra_threads` is the ONNX Runtime intra-op thread count for the session
/// (`None` = ORT's default); see [`embedding_session_intra_threads`].
#[allow(clippy::needless_return)]
fn resolve_provider(
    model: &str,
    backend: EmbeddingBackend,
    intra_threads: Option<usize>,
) -> Option<Arc<dyn EmbeddingProvider>> {
    #[cfg(not(feature = "onnxruntime"))]
    let _ = intra_threads;
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

    // Auto / Onnx: try ONNX Runtime first when it is compiled in.
    #[cfg(feature = "onnxruntime")]
    {
        if let Some(p) = OnnxProvider::try_with_model_intra(specs.onnx.clone(), intra_threads)
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

    // Auto fallback (ONNX unavailable / not compiled): Ollama.
    #[cfg(feature = "ollama")]
    if backend != EmbeddingBackend::Onnx {
        return OllamaProvider::try_new(specs.ollama).map(|p| Arc::new(p) as _);
    }

    let _ = &specs;
    None
}

/// The configured embedding model as a single in-process ONNX session, **only
/// when its files are already in the model cache**; otherwise `None`.
///
/// For latency-bound callers (the UserPromptSubmit hook) that would rather do
/// without a model than wait: unlike [`resolve_engine_providers`] this never
/// downloads a model and never falls back to Ollama (whose client waits up to
/// its request timeout when no server answers). It loads nothing else — no
/// NLI, reranker or T5. Blocking: run it off the async runtime.
pub fn cached_onnx_embedding_provider(
    config: &crate::types::EngramConfig,
) -> Option<Arc<dyn EmbeddingProvider>> {
    #[cfg(feature = "onnxruntime")]
    {
        let specs = provider_specs(config.embeddings.provider.as_str())?;
        if !specs.onnx.is_cached() {
            return None;
        }
        OnnxProvider::try_with_model_intra(specs.onnx, None)
            .map(|p| Arc::new(p) as Arc<dyn EmbeddingProvider>)
    }
    #[cfg(not(feature = "onnxruntime"))]
    {
        let _ = config;
        None
    }
}

/// Intra-op threads for each ONNX session of an embedding pool:
/// `ENGRAMDB_ONNX_INTRA_THREADS` when set, else
/// [`EmbeddingsConfig::session_intra_threads`](crate::types::config::EmbeddingsConfig::session_intra_threads)
/// (ORT's default for a single session, `cores / pool` for a pool).
fn embedding_session_intra_threads(pool_size: usize, cores: usize) -> Option<usize> {
    #[cfg(feature = "onnxruntime")]
    if let Some(n) = crate::onnx_ep::intra_threads_override() {
        return Some(n);
    }
    crate::types::config::EmbeddingsConfig::session_intra_threads(pool_size, cores)
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
/// embedding model (always attempted) and, when enabled, the NLI, reranker and
/// T5 title models. It performs blocking model loading. One-shot callers (the
/// CLI) call it once and exit; long-lived ones go through [`ProviderCache`],
/// which loads each model through the same per-model builders below and
/// shares it between every project whose settings select it.
pub fn resolve_engine_providers(
    config: &crate::types::EngramConfig,
    backend_override: Option<EmbeddingBackend>,
    embedding_pool_size: usize,
) -> EngineProviders {
    let backend = resolve_backend(config.embeddings.backend, backend_override);
    let mut providers = EngineProviders {
        embedding: build_embedding_pool(
            config.embeddings.provider.as_str(),
            backend,
            config.embeddings.dimensions,
            embedding_pool_size,
        ),
        ..EngineProviders::default()
    };
    if config.nli.enabled {
        providers.nli = build_nli(&config.nli.model);
    }
    if config.rerank.enabled {
        providers.reranker = build_reranker(&config.rerank.model);
    }
    if config.title.strategy == crate::title::TitleStrategy::T5 {
        providers.title = build_t5_pool(t5_pool_request(config, embedding_pool_size));
    }
    providers
}

/// The T5 title pool size a caller asks for. The CLI passes
/// `embedding_pool_size == 1` (one-shot, no concurrency) — don't pay N× the
/// heavy T5 load there; long-lived daemon / MCP callers (pool > 1) pool T5 too.
pub(crate) fn t5_pool_request(config: &EngramConfig, embedding_pool_size: usize) -> usize {
    if embedding_pool_size <= 1 {
        1
    } else {
        config
            .title
            .resolved_pool_size(crate::types::config::available_cores())
    }
}

/// Build an embedding pool of `pool_size` independent sessions of one model.
///
/// Round-robining across several sessions keeps concurrent callers (the
/// daemon serving N agent sessions) from all serializing behind one session's
/// mutex. A first-load failure leaves the pool empty → embeddings disabled,
/// exactly as a single-load failure behaved before; a later session failing
/// just yields a smaller pool (graceful degradation). Blocking.
pub(crate) fn build_embedding_pool(
    provider_name: &str,
    backend: EmbeddingBackend,
    configured_dimensions: usize,
    pool_size: usize,
) -> Option<Arc<dyn EmbeddingProvider>> {
    let intra_threads =
        embedding_session_intra_threads(pool_size, crate::types::config::available_cores());
    let mut sessions = Vec::with_capacity(pool_size.max(1));
    for _ in 0..pool_size.max(1) {
        match resolve_provider(provider_name, backend, intra_threads) {
            Some(p) => sessions.push(p),
            None => break,
        }
    }
    let provider = crate::embeddings::PooledEmbeddingProvider::build(sessions)?;
    if provider.dimensions() != configured_dimensions {
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
            provider_name,
            provider.dimensions(),
            configured_dimensions,
            provider.dimensions(),
        );
    }
    Some(provider)
}

/// Load the NLI contradiction model. ONNX-Runtime-only. Blocking.
pub(crate) fn build_nli(model: &str) -> Option<Arc<dyn NliProvider>> {
    #[cfg(feature = "onnxruntime")]
    {
        match OnnxNliProvider::try_new(model) {
            Some(provider) => Some(Arc::new(provider)),
            None => {
                eprintln!("Warning: NLI contradiction detection enabled but model unavailable");
                None
            }
        }
    }
    #[cfg(not(feature = "onnxruntime"))]
    {
        let _ = model;
        eprintln!(
            "Warning: NLI contradiction detection is enabled but unavailable on a build \
             without ONNX Runtime; continuing without it"
        );
        None
    }
}

/// Load the cross-encoder reranker. ONNX-Runtime-only. Blocking.
pub(crate) fn build_reranker(model: &str) -> Option<Arc<dyn Reranker>> {
    #[cfg(feature = "onnxruntime")]
    {
        match LocalReranker::load(model) {
            Ok(reranker) => Some(reranker),
            Err(e) => {
                eprintln!("Warning: reranker init failed, continuing without: {}", e);
                None
            }
        }
    }
    #[cfg(not(feature = "onnxruntime"))]
    {
        let _ = model;
        eprintln!("Warning: reranker is enabled but unavailable on this build");
        None
    }
}

/// Build the T5 title generator pool. T5 is a quantized ONNX
/// encoder+decoder, so this is ONNX-Runtime-only and blocking.
///
/// Caching it (rather than building it per `create`) means the long-lived
/// daemon / MCP server loads it once instead of on every `create`, and under
/// concurrency, pooling it cuts the dominant create-path tail latency. Each
/// session's intra_threads is reduced so `pool × intra ≤ cores` (T5 sessions
/// are direct ORT sessions, unlike fastembed), capped at the Lever A sweet
/// spot.
pub(crate) fn build_t5_pool(pool_size: usize) -> Option<Arc<dyn crate::title::TitleGenerator>> {
    #[cfg(feature = "onnxruntime")]
    {
        let cores = crate::types::config::available_cores();
        let pool_size = pool_size.max(1);
        let intra = (cores / pool_size)
            .min(crate::onnx_ep::intra_threads())
            .max(1);
        let mut generators: Vec<Arc<dyn crate::title::TitleGenerator>> =
            Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            match crate::title::t5::T5TitleGenerator::try_new_with_intra(intra) {
                Some(gen) => generators.push(Arc::new(gen)),
                None => break,
            }
        }
        let pool = crate::title::PooledTitleGenerator::build(generators);
        if pool.is_none() {
            eprintln!("Warning: title strategy 't5' configured but the T5 model is unavailable");
        }
        pool
    }
    #[cfg(not(feature = "onnxruntime"))]
    {
        let _ = pool_size;
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pooled embedding sessions split the cores; a single session keeps
    /// ORT's default; `ENGRAMDB_ONNX_INTRA_THREADS` wins over both.
    /// (nextest runs each test in its own process, so the env change is
    /// isolated.)
    #[test]
    fn embedding_session_intra_threads_policy_and_override() {
        std::env::remove_var("ENGRAMDB_ONNX_INTRA_THREADS");
        assert_eq!(embedding_session_intra_threads(1, 8), None);
        assert_eq!(embedding_session_intra_threads(2, 4), Some(2));
        assert_eq!(embedding_session_intra_threads(4, 8), Some(2));

        #[cfg(feature = "onnxruntime")]
        {
            std::env::set_var("ENGRAMDB_ONNX_INTRA_THREADS", "3");
            assert_eq!(embedding_session_intra_threads(1, 8), Some(3));
            assert_eq!(embedding_session_intra_threads(4, 8), Some(3));
            std::env::remove_var("ENGRAMDB_ONNX_INTRA_THREADS");
        }
    }

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
        assert!(provider_specs("all-minilm-l6").is_some());
        assert!(provider_specs("all-minilm-l12").is_some());
        assert!(provider_specs("all-minilm-l12-fp32").is_some());
        assert!(provider_specs("all-minilm-l12-int8").is_some());
        assert!(provider_specs("all-minilm-l6-int8").is_some());
        assert!(provider_specs("nomic-embed-text").is_some());
        assert!(provider_specs("mxbai-embed-large").is_some());
        assert!(provider_specs("definitely-not-a-model").is_none());
    }

    /// The depth pins must select genuinely different models, and `all-minilm`
    /// must track the shipped default. Without this, a user pinning
    /// `all-minilm-l6` to avoid the L12 reindex would silently get L12 anyway.
    #[cfg(feature = "onnxruntime")]
    #[test]
    fn minilm_depth_pins_are_distinct_and_default_tracks_l12() {
        let l6 = expected_embedding_fingerprint(&onnx_config("all-minilm-l6")).unwrap();
        let l12 = expected_embedding_fingerprint(&onnx_config("all-minilm-l12")).unwrap();
        let default = expected_embedding_fingerprint(&onnx_config("all-minilm")).unwrap();
        assert_eq!(l6.model, "onnx/all-MiniLM-L6-v2-u8");
        assert_eq!(l12.model, "onnx/all-MiniLM-L12-v2-u8");
        assert_ne!(l6.model, l12.model);
        assert_eq!(default.model, l12.model, "default must be L12");
        // Same dimensionality across the family: swapping depth needs a
        // re-embed, never a schema or config change.
        assert_eq!(l6.dimensions, l12.dimensions);

        // The reproducible fp32 pin is the same model at a different
        // precision, and must fingerprint distinctly so a switch reindexes.
        let fp32 = expected_embedding_fingerprint(&onnx_config("all-minilm-l12-fp32")).unwrap();
        assert_eq!(fp32.model, "onnx/all-MiniLM-L12-v2");
        assert_ne!(fp32.model, l12.model);
        assert_eq!(fp32.dimensions, l12.dimensions);
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

    /// An unstamped store with NO vectors is a fresh project, not a "legacy
    /// store": the report must say Match with no warning (there is nothing
    /// stale to re-embed). Once unknown-vintage vectors exist, the Untracked
    /// warning — and its `reindex --embeddings-only` remediation — is real
    /// and must come back.
    #[tokio::test]
    async fn untracked_report_requires_existing_vectors() {
        use crate::storage::manifest::EmbeddingFingerprint;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &crate::storage::InMemoryRegistry::new())
            .await
            .unwrap();
        let current = EmbeddingFingerprint {
            model: "onnx/all-MiniLM-L6-v2-q".to_string(),
            dimensions: 384,
            composition: None,
        };

        // Fresh store: unstamped, zero vectors — quiet.
        let report = embedding_model_report(&store, Some(current.clone())).await;
        assert!(matches!(report.status, EmbeddingModelStatus::Match));
        assert!(report.warning.is_none(), "fresh store must not warn");

        // Same store with vectors of unknown vintage — the warning is real.
        let mut memory = crate::types::Memory::new(
            crate::types::MemoryType::Context,
            "legacy",
            "body",
            crate::types::Provenance::human(),
        );
        memory.visibility = crate::types::Visibility::Shared;
        store.create(&memory).await.unwrap();
        store
            .upsert_chunks(&memory.id, vec![vec![0.3f32; 384]])
            .await
            .unwrap();

        let report = embedding_model_report(&store, Some(current)).await;
        assert!(matches!(
            report.status,
            EmbeddingModelStatus::Untracked { .. }
        ));
        assert!(
            report.warning.is_some(),
            "unknown-vintage vectors must warn"
        );
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

    /// Pins the hook-safety guarantee the reranking-on-by-default change
    /// depends on: even with `[rerank].enabled = true` explicitly configured
    /// (the new shipped default, not just an omitted section), the hook
    /// construction path never wires a reranker, so `reranking_available()`
    /// must stay false. `EngineProviders::default()` has no reranker
    /// regardless of config — this is what keeps `PreToolUse` / `SessionStart`
    /// / `UserPromptSubmit` (which fire on every Read/Write/Edit) free of the
    /// ~150 ms/query cross-encoder cost.
    #[tokio::test]
    async fn hook_engine_never_reranks() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &crate::storage::InMemoryRegistry::new())
            .await
            .unwrap();
        let config_path = temp_dir.path().join(".engramdb").join("config.toml");

        let mut config = crate::types::EngramConfig::default();
        assert!(
            config.rerank.enabled,
            "test assumes the shipped default is already enabled=true; \
             writing it explicitly below either way so the assertion holds \
             even if that default ever changes back"
        );
        config.rerank.enabled = true;
        config.to_toml_file(&config_path).unwrap();

        let engine = build_engine_without_providers(store, &config_path).await;

        assert!(
            !engine.reranking_available(),
            "hook engine must never rerank, even with rerank.enabled = true in config"
        );
    }

    /// Every spec a config string can select must appear in
    /// `ALL_ONNX_EMBEDDING_SPECS`.
    ///
    /// That list is what maps a cached model directory back to the model that
    /// created it. A spec reachable from `provider_specs` but missing from the
    /// list would read as an unattributable directory — which downstream
    /// tooling is entitled to treat as unused and delete, forcing a re-download
    /// (or, worse, a re-embed if a store is pinned to it).
    #[cfg(feature = "onnxruntime")]
    #[test]
    fn every_configurable_embedding_spec_is_enumerable() {
        use crate::embeddings::ALL_ONNX_EMBEDDING_SPECS;
        for provider in [
            "onnx",
            "all-minilm",
            "all-minilm-l6",
            "all-minilm-l12",
            "all-minilm-l12-int8",
            "all-minilm-l12-fp32",
            "nomic-embed-text",
            "mxbai-embed-large",
        ] {
            let Some(specs) = provider_specs(provider) else {
                continue;
            };
            assert!(
                ALL_ONNX_EMBEDDING_SPECS
                    .iter()
                    .any(|s| s.name == specs.onnx.name),
                "provider '{provider}' selects spec '{}', which is missing from \
                 ALL_ONNX_EMBEDDING_SPECS",
                specs.onnx.name
            );
        }
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

    // P2: `cross_store_equalization_needed` must fire only on a *confirmed*
    // embedding-fingerprint difference between the project and a shared store —
    // the same-fingerprint (and unstamped-store) cases take the fast path.
    #[tokio::test]
    async fn cross_store_equalization_needed_detects_drift() {
        use crate::storage::manifest::{load_manifest, save_manifest, EmbeddingFingerprint};
        use crate::types::EngramConfig;

        // Stamp a shared store's manifest, routing group ids vs the global id
        // the same way `shared_store_fingerprint` does.
        async fn stamp(store_id: &str, fp: &EmbeddingFingerprint) {
            let dir = if crate::storage::paths::is_group_id(store_id) {
                crate::storage::paths::group_store_dir(store_id).unwrap()
            } else {
                crate::storage::paths::global_store_dir().unwrap()
            };
            let path = crate::storage::paths::project_dir(&dir).join("manifest.toml");
            let mut m = load_manifest(&path).await.unwrap();
            m.embedding = Some(fp.clone());
            save_manifest(&path, &m).await.unwrap();
        }

        let config = EngramConfig::default();
        // Skip when this build resolves no embedding model (nothing to drift).
        let Some(project_fp) = expected_embedding_fingerprint(&config) else {
            return;
        };

        // No shared stores → never needs equalization.
        assert!(!cross_store_equalization_needed(&config, &[]).await);

        // Freshly created but *unstamped* group store → conservative fast path
        // (we can't confirm drift, so we don't equalize).
        let gid_untracked = crate::storage::paths::compute_group_id("equalize-untracked");
        drop(MemoryStore::init_group(&gid_untracked).await.unwrap());
        assert!(shared_store_fingerprint(&gid_untracked).await.is_none());
        assert!(!cross_store_equalization_needed(&config, &[gid_untracked]).await);

        // Group stamped with the SAME fingerprint → fast path (false).
        let gid_same = crate::storage::paths::compute_group_id("equalize-same");
        drop(MemoryStore::init_group(&gid_same).await.unwrap());
        stamp(&gid_same, &project_fp).await;
        assert_eq!(
            shared_store_fingerprint(&gid_same).await,
            Some(project_fp.clone())
        );
        assert!(!cross_store_equalization_needed(&config, std::slice::from_ref(&gid_same)).await);

        // Group stamped with a DIFFERENT model → needs equalization (true).
        let gid_diff = crate::storage::paths::compute_group_id("equalize-diff");
        drop(MemoryStore::init_group(&gid_diff).await.unwrap());
        let drifted = EmbeddingFingerprint {
            model: format!("{}-X", project_fp.model),
            dimensions: project_fp.dimensions,
            composition: project_fp.composition.clone(),
        };
        stamp(&gid_diff, &drifted).await;
        assert!(cross_store_equalization_needed(&config, std::slice::from_ref(&gid_diff)).await);

        // Composition-only drift (same model + dims) → still needs equalization
        // (whole-fingerprint compare), because it skews cross-store ranking just
        // like a model change.
        let gid_comp = crate::storage::paths::compute_group_id("equalize-composition");
        drop(MemoryStore::init_group(&gid_comp).await.unwrap());
        let comp_drift = EmbeddingFingerprint {
            model: project_fp.model.clone(),
            dimensions: project_fp.dimensions,
            composition: Some(format!(
                "{}-alt",
                project_fp.composition.clone().unwrap_or_default()
            )),
        };
        stamp(&gid_comp, &comp_drift).await;
        assert!(
            cross_store_equalization_needed(&config, std::slice::from_ref(&gid_comp)).await,
            "composition-only drift must trigger equalization"
        );

        // Global store id routes through the `else` (global) branch of
        // `shared_store_fingerprint` — a drifted global store is detected via
        // GLOBAL_PROJECT_ID, not just group ids.
        use crate::storage::paths::GLOBAL_PROJECT_ID;
        drop(MemoryStore::open_global().await.unwrap());
        let global_id = GLOBAL_PROJECT_ID.to_string();
        stamp(&global_id, &drifted).await;
        assert_eq!(shared_store_fingerprint(&global_id).await, Some(drifted));
        assert!(
            cross_store_equalization_needed(&config, std::slice::from_ref(&global_id)).await,
            "a drifted global store must be detected via GLOBAL_PROJECT_ID"
        );

        // Mixed aligned + drifted → any single drift triggers equalization.
        assert!(cross_store_equalization_needed(&config, &[gid_same, gid_diff]).await);
    }
}
