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

#[cfg(feature = "onnxruntime")]
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use engram_types::DEFAULT_RERANK_MODEL;
#[cfg(feature = "onnxruntime")]
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};
#[cfg(feature = "onnxruntime")]
use std::sync::{Arc, Mutex};

/// A cross-encoder loaded from an explicit HuggingFace file rather than
/// fastembed's built-in registry.
///
/// fastembed hardcodes `onnx/model.onnx` for every [`RerankerModel`], so its
/// registry can only ever reach the fp32 export — the reranker never got the
/// int8 treatment the embedding path has had since Lever B. The upstream repos
/// *do* publish quantized ONNX files; this names one directly.
#[cfg(feature = "onnxruntime")]
#[derive(Debug, Clone, Copy)]
pub struct RerankModelSpec {
    /// Config name users select with `[rerank].model`.
    pub name: &'static str,
    /// HuggingFace repo id.
    pub repo: &'static str,
    /// ONNX file within the repo.
    pub model_file: &'static str,
}

/// `jina-reranker-v1-turbo-en`, uint8-quantized (~36.5 MB vs ~144 MB fp32).
///
/// Same quantization scheme as the shipped uint8 embedding model. Selectable as
/// `[rerank].model = "jina-turbo-q"`.
#[cfg(feature = "onnxruntime")]
pub const RERANK_JINA_TURBO_Q: RerankModelSpec = RerankModelSpec {
    name: "jina-turbo-q",
    repo: "jinaai/jina-reranker-v1-turbo-en",
    model_file: "onnx/model_uint8.onnx",
};

/// Every reranker reachable through an explicit HF file. Consulted before
/// fastembed's registry so a quantized name wins over a same-named built-in.
#[cfg(feature = "onnxruntime")]
pub const USER_DEFINED_RERANKERS: &[RerankModelSpec] = &[RERANK_JINA_TURBO_Q];

/// The shipped reranker, as a spec.
///
/// Mirrors [`crate::nli::DEFAULT_NLI_MODEL`] and `title::DEFAULT_T5_MODEL`:
/// the default is anchored to a **spec**, not a bare string, so the name, repo
/// and ONNX file can never disagree. `engram_types::DEFAULT_RERANK_MODEL` is
/// the same value flattened to a `&str` for `RerankConfig` (which lives in the
/// `types` foundation and cannot see this crate); a test in `mod.rs` pins the
/// two together, exactly as NLI does.
#[cfg(feature = "onnxruntime")]
pub const DEFAULT_RERANK_SPEC: RerankModelSpec = RERANK_JINA_TURBO_Q;

/// Reranker names fastembed's built-in registry can serve (fp32 only).
///
/// [`LocalReranker::load`] checks this before calling
/// [`resolve_reranker_model`], so that function only ever sees a name it
/// recognizes — an unknown name degrades to [`DEFAULT_RERANK_SPEC`] instead.
#[cfg(feature = "onnxruntime")]
pub const FASTEMBED_RERANKERS: &[&str] = &[
    "jina-reranker-v1-turbo-en",
    "jina-reranker-v2-base-multilingual",
    "bge-reranker-base",
    "bge-reranker-v2-m3",
];

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
        crate::ensure_onnx_runtime()?;
        let cache_dir =
            engram_storage::paths::model_cache_dir().map_err(|e| anyhow::anyhow!("{}", e))?;

        // An explicit-file spec (e.g. the quantized export) wins over
        // fastembed's registry, which can only reach `onnx/model.onnx`.
        if let Some(spec) = USER_DEFINED_RERANKERS.iter().find(|s| s.name == model_name) {
            ensure_cached_when_offline(spec.name, spec.repo)?;
            let reranker = Self::load_user_defined(spec, cache_dir)?;
            return Ok(Self::shared(Arc::new(Mutex::new(reranker))));
        }

        // Unrecognized name: degrade to the shipped default rather than to a
        // separate fp32 constant. An invalid config should land on the same
        // model a fresh config would, not on a third behaviour nobody chose.
        if !FASTEMBED_RERANKERS.contains(&model_name) {
            tracing::warn!(
                "unknown rerank.model '{}'; falling back to the default '{}' \
                 (known: {}, {})",
                model_name,
                DEFAULT_RERANK_MODEL,
                DEFAULT_RERANK_SPEC.name,
                FASTEMBED_RERANKERS.join(", ")
            );
            ensure_cached_when_offline(DEFAULT_RERANK_SPEC.name, DEFAULT_RERANK_SPEC.repo)?;
            let reranker = Self::load_user_defined(&DEFAULT_RERANK_SPEC, cache_dir)?;
            return Ok(Self::shared(Arc::new(Mutex::new(reranker))));
        }

        let model = resolve_reranker_model(model_name);
        // fastembed keeps the repo id in the registry rather than in a spec of
        // ours, so resolve it there (same shape as the embedding loader).
        ensure_cached_when_offline(model_name, &TextRerank::get_model_info(&model).model_code)?;
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

    /// Load a cross-encoder from an explicit HuggingFace file.
    ///
    /// Mirrors `OnnxProvider::load_user_defined`: `hf_hub` fetches (or serves
    /// from cache) the named ONNX file plus the tokenizer set, and fastembed's
    /// user-defined seam builds the session. Files land in the same unified
    /// model cache as every other model.
    fn load_user_defined(
        spec: &RerankModelSpec,
        cache_dir: std::path::PathBuf,
    ) -> Result<TextRerank> {
        use fastembed::{
            OnnxSource, RerankInitOptionsUserDefined, TokenizerFiles, UserDefinedRerankingModel,
        };

        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache_dir)
            .build()
            .context("init HuggingFace API")?;
        let repo = api.model(spec.repo.to_string());
        let read = |file: &str| -> Result<Vec<u8>> {
            let path = repo
                .get(file)
                .with_context(|| format!("fetch {}/{file}", spec.repo))?;
            std::fs::read(&path).with_context(|| format!("read {}", path.display()))
        };

        let onnx_file = read(spec.model_file)?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };

        let model = UserDefinedRerankingModel::new(OnnxSource::Memory(onnx_file), tokenizer_files);
        let mut options = RerankInitOptionsUserDefined::default();
        let eps = engram_onnx::execution_providers();
        if !eps.is_empty() {
            options = options.with_execution_providers(eps);
        }
        TextRerank::try_new_from_user_defined(model, options)
            .map_err(|e| anyhow::anyhow!("{}", e))
            .with_context(|| format!("Failed to initialize reranker '{}'", spec.name))
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

/// Map a reranker model name string to a fastembed `RerankerModel` variant.
///
/// [`LocalReranker::load`] gates this behind [`FASTEMBED_RERANKERS`], so every
/// name reaching here is known. The catch-all arm is defensive only (keeping
/// the two lists in sync is enforced by a test) and mirrors the gate's
/// behaviour by warning rather than silently substituting.
#[cfg(feature = "onnxruntime")]
/// Refuse to download an uncached cross-encoder in offline mode.
///
/// The embedding, NLI and T5 loaders all gate on
/// [`engram_storage::paths::offline_enabled`]; the reranker did not, so the
/// documented "empty `ENGRAMDB_MODEL_CACHE_DIR` + `ENGRAMDB_OFFLINE`" recipe
/// for simulating a missing model silently did nothing here. A developer with
/// the cross-encoder already in the shared cache loaded it anyway, while a
/// cold CI runner tried to download it — so tests asserting model
/// *unavailability* disagreed between machines.
fn ensure_cached_when_offline(model_name: &str, repo: &str) -> Result<()> {
    if engram_storage::paths::offline_enabled() && !engram_storage::paths::hf_repo_cached(repo) {
        anyhow::bail!(
            "offline mode (ENGRAMDB_OFFLINE) and reranker model '{model_name}' ({repo}) is not cached"
        );
    }
    Ok(())
}

fn resolve_reranker_model(name: &str) -> RerankerModel {
    match name {
        "bge-reranker-v2-m3" => RerankerModel::BGERerankerV2M3,
        "jina-reranker-v1-turbo-en" => RerankerModel::JINARerankerV1TurboEn,
        "jina-reranker-v2-base-multilingual" => RerankerModel::JINARerankerV2BaseMultiligual,
        "bge-reranker-base" => RerankerModel::BGERerankerBase,
        // Unreachable via `load` (gated by `FASTEMBED_RERANKERS`); defensive
        // only, and pinned by `fastembed_registry_list_matches_match_arms`.
        other => {
            tracing::warn!("unrecognized fastembed reranker '{other}'; using the fp32 jina turbo");
            RerankerModel::JINARerankerV1TurboEn
        }
    }
}

#[cfg(all(test, feature = "onnxruntime"))]
mod tests {
    use super::*;

    /// Offline mode must refuse an uncached cross-encoder instead of
    /// downloading it, matching the embedding/NLI/T5 loaders. Without this the
    /// documented "empty cache dir + `ENGRAMDB_OFFLINE`" recipe for simulating
    /// a missing model did nothing for the reranker, so tests asserting model
    /// unavailability passed or failed depending on what the machine had
    /// cached. Nextest runs each test in its own process, so setting the env
    /// vars here cannot leak into another test.
    #[test]
    fn offline_mode_refuses_an_uncached_reranker() {
        let empty_cache = tempfile::tempdir().expect("tempdir");
        std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", empty_cache.path());
        std::env::set_var("ENGRAMDB_OFFLINE", "1");

        let err = ensure_cached_when_offline("jina-turbo-q", "jinaai/jina-reranker-v1-turbo-en")
            .expect_err("an uncached model must fail fast in offline mode");
        let msg = err.to_string();
        assert!(
            msg.contains("ENGRAMDB_OFFLINE") && msg.contains("not cached"),
            "error should name the offline switch and the cause, got: {msg}"
        );
    }

    /// The guard is inert when offline mode is off — an uncached model is
    /// still allowed to download, which is the normal path.
    #[test]
    fn online_mode_allows_an_uncached_reranker() {
        let empty_cache = tempfile::tempdir().expect("tempdir");
        std::env::set_var("ENGRAMDB_MODEL_CACHE_DIR", empty_cache.path());
        std::env::remove_var("ENGRAMDB_OFFLINE");

        ensure_cached_when_offline("jina-turbo-q", "jinaai/jina-reranker-v1-turbo-en")
            .expect("online mode must not block an uncached model");
    }

    /// The quantized spec must be reachable by its config name.
    ///
    /// This is the guard against a silent downgrade: `resolve_reranker_model`
    /// maps every *unknown* name to the fp32 default with only a warning, so if
    /// `load` ever consulted it before the spec table, `jina-turbo-q` would
    /// quietly load the 144 MB fp32 model instead of the 36 MB uint8 one — the
    /// exact thing this spec exists to avoid, and invisible at runtime.
    #[test]
    fn quantized_spec_is_reachable_by_name() {
        let spec = USER_DEFINED_RERANKERS
            .iter()
            .find(|s| s.name == "jina-turbo-q")
            .expect("jina-turbo-q must resolve to an explicit-file spec");
        assert_eq!(spec.repo, "jinaai/jina-reranker-v1-turbo-en");
        assert!(
            spec.model_file.contains("uint8"),
            "spec must name a quantized export, got {}",
            spec.model_file
        );
    }

    /// The shipped default is anchored to a spec, and an invalid config lands
    /// on that same default rather than a third behaviour nobody chose.
    #[test]
    fn default_is_spec_anchored_and_is_the_invalid_config_fallback() {
        assert_eq!(
            DEFAULT_RERANK_SPEC.name, DEFAULT_RERANK_MODEL,
            "the flattened `types` string must match the spec it stands for"
        );
        assert!(
            USER_DEFINED_RERANKERS
                .iter()
                .any(|s| s.name == DEFAULT_RERANK_SPEC.name),
            "the default spec must be reachable through the spec table"
        );
        assert!(
            !FASTEMBED_RERANKERS.contains(&DEFAULT_RERANK_MODEL),
            "the default is a user-defined spec, so `load` must not route it \
             through fastembed's registry"
        );
    }

    /// `FASTEMBED_RERANKERS` gates `resolve_reranker_model`, so any name in the
    /// list must have a real match arm — otherwise `load` would admit a name
    /// that then silently hits the defensive catch-all.
    #[test]
    fn fastembed_registry_list_matches_match_arms() {
        for name in FASTEMBED_RERANKERS {
            let resolved = resolve_reranker_model(name);
            let fallback = resolve_reranker_model("definitely-not-a-model");
            if *name != "jina-reranker-v1-turbo-en" {
                assert_ne!(
                    format!("{resolved:?}"),
                    format!("{fallback:?}"),
                    "'{name}' is listed as a fastembed reranker but has no match arm"
                );
            }
        }
    }

    /// Every user-defined name must be distinct from fastembed's registry
    /// names, or `load`'s table-first lookup would shadow a built-in model.
    #[test]
    fn user_defined_names_do_not_shadow_builtins() {
        const BUILTIN: &[&str] = &[
            "jina-reranker-v1-turbo-en",
            "jina-reranker-v2-base-multilingual",
            "bge-reranker-base",
            "bge-reranker-v2-m3",
        ];
        for spec in USER_DEFINED_RERANKERS {
            assert!(
                !BUILTIN.contains(&spec.name),
                "'{}' shadows a fastembed built-in",
                spec.name
            );
        }
    }
}
