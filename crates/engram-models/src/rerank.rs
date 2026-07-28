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
            let reranker = Self::load_user_defined(spec, cache_dir)?;
            return Ok(Self::shared(Arc::new(Mutex::new(reranker))));
        }

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

/// Map a reranker model name string to a fastembed `RerankerModel` enum variant.
///
/// The recognized default name is [`DEFAULT_RERANK_MODEL`]; anything else
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
                "unknown rerank.model '{}'; falling back to {} \
                 (known: jina-reranker-v1-turbo-en, bge-reranker-base, bge-reranker-v2-m3, \
                 jina-reranker-v2-base-multilingual)",
                other,
                DEFAULT_RERANK_MODEL
            );
            RerankerModel::JINARerankerV1TurboEn
        }
    }
}

#[cfg(all(test, feature = "onnxruntime"))]
mod tests {
    use super::*;

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
