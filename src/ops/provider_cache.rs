//! Process-wide cache of model-backed providers, one entry per **model**.
//!
//! The daemon serves many projects, and a project's config selects up to four
//! models: an embedding model, an NLI model, a reranker and the T5 title
//! model. The cache used to hold one *bundle* per distinct combination, so two
//! projects that differed in any model setting — `[nli].enabled`,
//! `[title].strategy`, a pool size — each loaded their own copy of every
//! model, the embedding pool included. Now each model has its own entry, keyed
//! only by what identifies that model ([`ModelKey`]), and a project's
//! [`EngineProviders`] is assembled from the shared entries.
//!
//! Pooled models (the embedding model and T5) are shared at one size: the
//! largest pool size any live project asks for, rounded up to a power of two
//! and capped at the core count ([`pool_class`]). Requests of 1, 2 and 3 share
//! a pool of 4; when the project that asked for 3 goes away, the pool shrinks
//! to 2. Rounding to powers of two means a pool is rebuilt only when the
//! largest request crosses 1, 2, 4, 8 …, not on every small change.
//!
//! A resize **rebuilds** the pool rather than adding or dropping sessions:
//! each session is built with `cores / pool_size` intra-op threads, so
//! sessions from pools of different sizes must not be mixed (that
//! oversubscribes the cores, which the repo measured as slower than one
//! session). Growing happens in the background — the current pool keeps
//! serving until the larger one replaces it — so no request waits on the
//! extra load. Only a model's first load is synchronous.
//!
//! Every request and every lease ([`ProviderCache::touch`]) records a
//! *demand*: the pool size it asked for and when. [`ProviderCache::evict_idle`]
//! drops demands older than the idle window, shrinks pools whose largest
//! demand went away, and unloads models with no demand left — unless a caller
//! still holds the model.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{
    build_embedding_pool, build_nli, build_reranker, build_t5_pool, resolve_backend,
    t5_pool_request, EngineProviders,
};
use crate::embeddings::EmbeddingProvider;
use crate::nli::NliProvider;
use crate::retrieval::reranker::Reranker;
use crate::title::TitleGenerator;
use crate::types::{EmbeddingBackend, EngramConfig};

/// How long a model that failed to load is reported as unavailable before
/// the next caller re-attempts the load. Long enough that a broken machine
/// isn't paying a failed load per request, short enough that fixing the model
/// cache heals live daemons/sessions promptly — the same "cached a `None`
/// failure forever" class `DaemonCell` fixed for daemon handles.
const FAILED_LOAD_RETRY_AFTER: Duration = Duration::from_secs(30);

/// What identifies one loaded model. Two configs that produce an equal key
/// share the loaded model, whatever else differs between them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ModelKey {
    /// The embedding model. `backend` is the *resolved* backend, so an
    /// override and the config default never collide.
    Embedding {
        backend: EmbeddingBackend,
        provider: String,
        dimensions: usize,
    },
    Nli {
        model: String,
    },
    Rerank {
        model: String,
    },
    /// The T5 title model. There is one T5 model, so nothing else to key on.
    Title,
}

impl ModelKey {
    /// Whether this model is served as a pool of sessions whose size is
    /// negotiated between projects.
    fn pooled(&self) -> bool {
        matches!(self, ModelKey::Embedding { .. } | ModelKey::Title)
    }
}

/// The models `config` selects, each with the pool size it asks for (`1` for
/// the unpooled NLI and reranker models).
///
/// The four model-selecting config sections are destructured
/// **exhaustively** (no `..`): adding a field to any of them fails
/// compilation here, forcing an explicit decision about whether the new field
/// changes which model is loaded. The cache key drifted behind the config
/// twice before (title.strategy, title.pool_size — each a stale-bundle bug).
/// If a new field belongs in a key, also make sure `daemon::ModelSelection`
/// carries it, or the daemon will key (and load) models without it.
pub(crate) fn model_demands(
    config: &EngramConfig,
    backend_override: Option<EmbeddingBackend>,
    embedding_pool_size: usize,
) -> Vec<(ModelKey, usize)> {
    let crate::types::config::EmbeddingsConfig {
        backend: config_backend,
        provider,
        dimensions,
        // Chunking width, not a model identity: the same loaded session
        // serves any max_tokens (effective_chunk_tokens clamps per call).
        max_tokens: _,
        // Reindex *policy* — what to do on a model change, not which model.
        reindex_on_model_change: _,
        // Embed-text *composition*, applied per embed call — the same loaded
        // session serves both compositions; not model identity.
        metadata_vector: _,
        // A demand, not an identity: the caller passes the RESOLVED pool
        // size, and pools are shared at the largest demand.
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
        // A demand, not an identity: read through `t5_pool_request`.
        pool_size: _,
    } = &config.title;

    let backend = resolve_backend(*config_backend, backend_override);
    let mut demands = vec![(
        ModelKey::Embedding {
            backend,
            provider: provider.clone(),
            dimensions: *dimensions,
        },
        embedding_pool_size.max(1),
    )];
    if *nli_enabled {
        demands.push((
            ModelKey::Nli {
                model: nli_model.clone(),
            },
            1,
        ));
    }
    if *rerank_enabled {
        demands.push((
            ModelKey::Rerank {
                model: rerank_model.clone(),
            },
            1,
        ));
    }
    if *title_strategy == crate::title::TitleStrategy::T5 {
        demands.push((
            ModelKey::Title,
            t5_pool_request(config, embedding_pool_size),
        ));
    }
    demands
}

/// Signature of everything model-related `config` asks for: which models,
/// and the pool size requested for each.
///
/// The cache does not key on this — it keys each model separately — but two
/// configs with an equal signature are guaranteed to be served identically,
/// which is what the daemon tests use it to check (a `ModelSelection` must
/// carry every field that reaches it).
pub fn provider_cache_key(
    config: &EngramConfig,
    backend_override: Option<EmbeddingBackend>,
    embedding_pool_size: usize,
) -> String {
    model_demands(config, backend_override, embedding_pool_size)
        .iter()
        .map(|(key, pool)| format!("{key:?}x{pool}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// The pool size a pooled model is loaded at for a requested size: the next
/// power of two, capped at the core count (as `pool_size` always was).
pub(crate) fn pool_class(requested: usize, cores: usize) -> usize {
    requested.max(1).next_power_of_two().min(cores.max(1))
}

/// A loaded model.
#[derive(Clone)]
pub(crate) enum ModelHandle {
    Embedding(Arc<dyn EmbeddingProvider>),
    Nli(Arc<dyn NliProvider>),
    Rerank(Arc<dyn Reranker>),
    Title(Arc<dyn TitleGenerator>),
}

impl ModelHandle {
    /// True when a caller other than the cache still holds this model — an
    /// in-flight request, or an engine assembled from it.
    fn in_use(&self) -> bool {
        match self {
            ModelHandle::Embedding(p) => Arc::strong_count(p) > 1,
            ModelHandle::Nli(p) => Arc::strong_count(p) > 1,
            ModelHandle::Rerank(p) => Arc::strong_count(p) > 1,
            ModelHandle::Title(p) => Arc::strong_count(p) > 1,
        }
    }
}

/// Loads one model. The production loader is [`OnnxLoader`]; tests inject a
/// fake so the cache's sharing, sizing and eviction are testable without a
/// model on disk.
pub(crate) trait ModelLoader: Send + Sync + 'static {
    /// Load `key` as a pool of `pool_size` sessions (ignored for unpooled
    /// models). Blocking; `None` when the model is unavailable.
    fn load(&self, key: &ModelKey, pool_size: usize) -> Option<ModelHandle>;
}

/// Loads models with the same builders [`super::resolve_engine_providers`]
/// uses.
struct OnnxLoader;

impl ModelLoader for OnnxLoader {
    fn load(&self, key: &ModelKey, pool_size: usize) -> Option<ModelHandle> {
        match key {
            ModelKey::Embedding {
                backend,
                provider,
                dimensions,
            } => build_embedding_pool(provider, *backend, *dimensions, pool_size)
                .map(ModelHandle::Embedding),
            ModelKey::Nli { model } => build_nli(model).map(ModelHandle::Nli),
            ModelKey::Rerank { model } => build_reranker(model).map(ModelHandle::Rerank),
            ModelKey::Title => build_t5_pool(pool_size).map(ModelHandle::Title),
        }
    }
}

/// One model's entry. Entries are never removed from the map — an unloaded
/// model leaves an empty entry of a few bytes — because removing one would
/// race a caller that already holds the entry and is about to load into it,
/// leaving a loaded model the cache no longer tracks.
struct Slot {
    key: ModelKey,
    /// Single-flight for loads and resizes of this model only, so a slow T5
    /// load never stalls an embedding lookup.
    load: tokio::sync::Mutex<()>,
    state: Mutex<SlotState>,
}

#[derive(Default)]
struct SlotState {
    /// The loaded model and the pool size it was built at.
    loaded: Option<(ModelHandle, usize)>,
    /// When the last load failed, while the model stays unloaded.
    failed_at: Option<Instant>,
    /// Requested pool size → when it was last requested or leased.
    demands: HashMap<usize, Instant>,
    /// A background resize is queued or running.
    resizing: bool,
}

impl SlotState {
    /// The pool size this model should be loaded at now.
    fn target(&self, key: &ModelKey, cores: usize) -> usize {
        if !key.pooled() {
            return 1;
        }
        let largest = self.demands.keys().copied().max().unwrap_or(1);
        pool_class(largest, cores)
    }
}

struct Inner {
    loader: Arc<dyn ModelLoader>,
    cores: usize,
    slots: Mutex<HashMap<ModelKey, Arc<Slot>>>,
    /// Models (or replaced pools) let go of since the last
    /// [`ProviderCache::take_released`] — the daemon trims the heap then.
    released: AtomicUsize,
}

/// Process-wide cache of loaded models, shared by every project whose config
/// selects them. Backs both the shared daemon and the in-process MCP fallback.
#[derive(Clone)]
pub struct ProviderCache {
    inner: Arc<Inner>,
}

impl Default for ProviderCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Lock a std mutex, recovering the data if a panicking thread poisoned it:
/// every critical section here leaves the state consistent.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ProviderCache {
    /// Create an empty cache that loads real models.
    pub fn new() -> Self {
        Self::with_loader(
            Arc::new(OnnxLoader),
            crate::types::config::available_cores(),
        )
    }

    /// Create an empty cache with a custom loader and core count.
    pub(crate) fn with_loader(loader: Arc<dyn ModelLoader>, cores: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                loader,
                cores,
                slots: Mutex::new(HashMap::new()),
                released: AtomicUsize::new(0),
            }),
        }
    }

    fn slot(&self, key: &ModelKey) -> Arc<Slot> {
        let mut slots = lock(&self.inner.slots);
        Arc::clone(slots.entry(key.clone()).or_insert_with(|| {
            Arc::new(Slot {
                key: key.clone(),
                load: tokio::sync::Mutex::new(()),
                state: Mutex::new(SlotState::default()),
            })
        }))
    }

    fn all_slots(&self) -> Vec<Arc<Slot>> {
        lock(&self.inner.slots).values().cloned().collect()
    }

    /// Number of models currently loaded.
    pub async fn loaded_count(&self) -> usize {
        self.all_slots()
            .iter()
            .filter(|s| lock(&s.state).loaded.is_some())
            .count()
    }

    /// Every loaded model, as `"<role>=<identity>"`, with ` x<N>` for a pool
    /// of N sessions.
    ///
    /// Embeddings report the provider's `model_id()` — the identity persisted
    /// in the manifest, which can silently corrupt a store when a stale
    /// daemon serves vectors from a superseded model. Sorted so `Status`
    /// output is stable across calls.
    pub async fn model_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .all_slots()
            .iter()
            .filter_map(|slot| {
                let st = lock(&slot.state);
                let (handle, size) = st.loaded.as_ref()?;
                Some(match (handle, &slot.key) {
                    (ModelHandle::Embedding(p), _) => format!("embed={} x{size}", p.model_id()),
                    (ModelHandle::Nli(_), ModelKey::Nli { model }) => format!("nli={model}"),
                    (ModelHandle::Rerank(_), ModelKey::Rerank { model }) => {
                        format!("rerank={model}")
                    }
                    (ModelHandle::Title(_), _) => format!("title=t5 x{size}"),
                    (ModelHandle::Nli(_), _) => "nli=loaded".to_string(),
                    (ModelHandle::Rerank(_), _) => "rerank=loaded".to_string(),
                })
            })
            .collect();
        ids.sort();
        ids
    }

    /// Providers for `config`, loading each selected model at most once per
    /// process and sharing it with every other config that selects it.
    pub async fn get(
        &self,
        config: &EngramConfig,
        backend_override: Option<EmbeddingBackend>,
    ) -> EngineProviders {
        let pool_size = config.embeddings.resolved_pool_size(self.inner.cores);
        let mut providers = EngineProviders::default();
        for (key, want) in model_demands(config, backend_override, pool_size) {
            let slot = self.slot(&key);
            match self.ensure(&slot, want).await {
                Some(ModelHandle::Embedding(p)) => providers.embedding = Some(p),
                Some(ModelHandle::Nli(p)) => providers.nli = Some(p),
                Some(ModelHandle::Rerank(p)) => providers.reranker = Some(p),
                Some(ModelHandle::Title(p)) => providers.title = Some(p),
                None => {}
            }
        }
        providers
    }

    /// Record a demand of `want` on `slot` and return its model, loading it
    /// if this is the first demand (or a past failure has aged out).
    async fn ensure(&self, slot: &Arc<Slot>, want: usize) -> Option<ModelHandle> {
        {
            let mut st = lock(&slot.state);
            st.demands.insert(want, Instant::now());
            if let Some((handle, size)) = &st.loaded {
                let handle = handle.clone();
                // Grow in the background; shrinking is the sweeper's call.
                if st.target(&slot.key, self.inner.cores) > *size && !st.resizing {
                    st.resizing = true;
                    self.spawn_resize(Arc::clone(slot));
                }
                return Some(handle);
            }
            if st
                .failed_at
                .is_some_and(|t| t.elapsed() < FAILED_LOAD_RETRY_AFTER)
            {
                return None;
            }
        }

        let _single_flight = slot.load.lock().await;
        let target = {
            let st = lock(&slot.state);
            // Someone else loaded (or failed) while we waited.
            if let Some((handle, _)) = &st.loaded {
                return Some(handle.clone());
            }
            if st
                .failed_at
                .is_some_and(|t| t.elapsed() < FAILED_LOAD_RETRY_AFTER)
            {
                return None;
            }
            st.target(&slot.key, self.inner.cores)
        };
        let loaded = self.load_blocking(&slot.key, target).await;
        let mut st = lock(&slot.state);
        match loaded {
            Some(handle) => {
                st.loaded = Some((handle.clone(), target));
                st.failed_at = None;
                Some(handle)
            }
            None => {
                tracing::warn!(
                    "model {:?} failed to load; will re-attempt after {}s",
                    slot.key,
                    FAILED_LOAD_RETRY_AFTER.as_secs()
                );
                st.failed_at = Some(Instant::now());
                None
            }
        }
    }

    async fn load_blocking(&self, key: &ModelKey, pool_size: usize) -> Option<ModelHandle> {
        let loader = Arc::clone(&self.inner.loader);
        let key = key.clone();
        tokio::task::spawn_blocking(move || loader.load(&key, pool_size))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("model load task panicked: {e}");
                None
            })
    }

    /// Rebuild `slot`'s pool at its current target size, in the background.
    /// The caller has set `resizing`.
    fn spawn_resize(&self, slot: Arc<Slot>) {
        let cache = self.clone();
        tokio::spawn(async move { cache.resize(slot).await });
    }

    async fn resize(&self, slot: Arc<Slot>) {
        let _single_flight = slot.load.lock().await;
        let target = {
            let mut st = lock(&slot.state);
            match &st.loaded {
                Some((_, size)) if st.target(&slot.key, self.inner.cores) != *size => {
                    st.target(&slot.key, self.inner.cores)
                }
                // Unloaded meanwhile, or demand already back at this size.
                _ => {
                    st.resizing = false;
                    return;
                }
            }
        };
        let rebuilt = self.load_blocking(&slot.key, target).await;
        let mut st = lock(&slot.state);
        st.resizing = false;
        match rebuilt {
            // Swap only if the model is still loaded: one evicted while this
            // rebuild ran must stay unloaded. The replaced pool is freed when
            // the last in-flight request holding it finishes.
            Some(handle) if st.loaded.is_some() => {
                st.loaded = Some((handle, target));
                self.inner.released.fetch_add(1, Ordering::Relaxed);
            }
            Some(_) => {}
            None => tracing::warn!(
                "resizing {:?} to {target} sessions failed; keeping the current pool",
                slot.key
            ),
        }
    }

    /// Renew the demands `config` makes on the models it selects, without
    /// loading anything.
    ///
    /// This is the lease a live session renews: the daemon calls it for the
    /// model selection a heartbeat `Ping` carries (and after serving each
    /// request), so a session that is open but quiet keeps its models — at
    /// its pool size — resident. Returns `false` when none of the models is
    /// known to the cache.
    pub async fn touch(
        &self,
        config: &EngramConfig,
        backend_override: Option<EmbeddingBackend>,
    ) -> bool {
        let pool_size = config.embeddings.resolved_pool_size(self.inner.cores);
        let slots = lock(&self.inner.slots);
        let mut known = false;
        for (key, want) in model_demands(config, backend_override, pool_size) {
            if let Some(slot) = slots.get(&key) {
                let mut st = lock(&slot.state);
                if st.loaded.is_some() || !st.demands.is_empty() {
                    st.demands.insert(want, Instant::now());
                    known = true;
                }
            }
        }
        known
    }

    /// Expire demands older than `max_idle`, shrink pools whose largest
    /// demand expired, and unload models with no demand left.
    ///
    /// This is how a project is "closed": when the last session using a
    /// model ends, its leases stop, and the model — every ONNX session in
    /// it, with each session's memory arena — is released here. A model some
    /// caller still holds is kept, however old its last use: a request that
    /// runs longer than `max_idle` must not have its model dropped under it.
    /// Shrinks run in the background. Returns how many models were unloaded.
    pub async fn evict_idle(&self, max_idle: Duration) -> usize {
        let mut unloaded = 0;
        for slot in self.all_slots() {
            let mut st = lock(&slot.state);
            st.demands.retain(|_, at| at.elapsed() < max_idle);
            let Some((handle, size)) = &st.loaded else {
                continue;
            };
            if st.demands.is_empty() {
                if !handle.in_use() {
                    st.loaded = None;
                    unloaded += 1;
                    self.inner.released.fetch_add(1, Ordering::Relaxed);
                }
            } else if st.target(&slot.key, self.inner.cores) < *size && !st.resizing {
                st.resizing = true;
                self.spawn_resize(Arc::clone(&slot));
            }
        }
        unloaded
    }

    /// How many models or replaced pools were let go of since the last call.
    /// The daemon returns freed memory to the OS when this is non-zero.
    pub fn take_released(&self) -> usize {
        self.inner.released.swap(0, Ordering::Relaxed)
    }
}

/// A loader that loads fake models, for tests in this crate.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// An embedding "model" whose `model_id` names its pool size, so a test
    /// can tell which pool served it.
    pub(crate) struct FakeEmbedding(pub usize);

    #[async_trait::async_trait]
    impl EmbeddingProvider for FakeEmbedding {
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
            format!("test/fake-x{}", self.0)
        }
    }

    pub(crate) struct FakeTitle;

    #[async_trait::async_trait]
    impl TitleGenerator for FakeTitle {
        async fn generate(&self, _text: &str) -> anyhow::Result<String> {
            Ok("title".to_string())
        }
    }

    pub(crate) struct FakeReranker;

    #[async_trait::async_trait]
    impl Reranker for FakeReranker {
        async fn rerank(
            &self,
            _query: &str,
            documents: &[String],
        ) -> anyhow::Result<Vec<crate::retrieval::reranker::RerankScore>> {
            Ok((0..documents.len())
                .map(|index| crate::retrieval::reranker::RerankScore { index, score: 0.0 })
                .collect())
        }
    }

    /// Records every load; loads fail while `fail` is set. NLI is never
    /// loadable (tests don't need it).
    #[derive(Default)]
    pub(crate) struct FakeLoader {
        pub loads: Mutex<Vec<(ModelKey, usize)>>,
        pub fail: std::sync::atomic::AtomicBool,
    }

    impl FakeLoader {
        pub(crate) fn loads_of(&self, pred: impl Fn(&ModelKey) -> bool) -> Vec<usize> {
            lock(&self.loads)
                .iter()
                .filter(|(k, _)| pred(k))
                .map(|(_, n)| *n)
                .collect()
        }
    }

    impl ModelLoader for FakeLoader {
        fn load(&self, key: &ModelKey, pool_size: usize) -> Option<ModelHandle> {
            lock(&self.loads).push((key.clone(), pool_size));
            if self.fail.load(Ordering::SeqCst) {
                return None;
            }
            match key {
                ModelKey::Embedding { .. } => {
                    Some(ModelHandle::Embedding(Arc::new(FakeEmbedding(pool_size))))
                }
                ModelKey::Title => Some(ModelHandle::Title(Arc::new(FakeTitle))),
                ModelKey::Rerank { .. } => Some(ModelHandle::Rerank(Arc::new(FakeReranker))),
                ModelKey::Nli { .. } => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeLoader;
    use super::*;

    /// A config with only the embedding model, at an explicit pool size.
    fn embed_only(pool: usize) -> EngramConfig {
        let mut c = EngramConfig::default();
        c.nli.enabled = false;
        c.rerank.enabled = false;
        c.title.strategy = crate::title::TitleStrategy::Keyword;
        c.embeddings.pool_size = Some(pool);
        c
    }

    fn is_embedding(k: &ModelKey) -> bool {
        matches!(k, ModelKey::Embedding { .. })
    }

    fn fake_cache(cores: usize) -> (Arc<FakeLoader>, ProviderCache) {
        let loader = Arc::new(FakeLoader::default());
        let cache = ProviderCache::with_loader(loader.clone(), cores);
        (loader, cache)
    }

    /// Wait (bounded) until `cond` holds; background resizes finish on
    /// their own schedule.
    async fn eventually(mut cond: impl FnMut() -> bool) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition never held");
    }

    /// The pool size the embedding model is loaded at, if loaded.
    fn embedding_pool(cache: &ProviderCache) -> Option<usize> {
        cache
            .all_slots()
            .iter()
            .find_map(|s| match (&s.key, &lock(&s.state).loaded) {
                (ModelKey::Embedding { .. }, Some((_, size))) => Some(*size),
                _ => None,
            })
    }

    async fn embed_id(cache: &ProviderCache, pool: usize) -> String {
        cache
            .get(&embed_only(pool), None)
            .await
            .embedding
            .expect("embedding")
            .model_id()
    }

    #[test]
    fn pool_class_rounds_up_to_a_power_of_two_capped_at_cores() {
        assert_eq!(pool_class(0, 8), 1);
        assert_eq!(pool_class(1, 8), 1);
        assert_eq!(pool_class(2, 8), 2);
        assert_eq!(pool_class(3, 8), 4);
        assert_eq!(pool_class(5, 8), 8);
        assert_eq!(pool_class(9, 16), 16);
        // The cap wins over the next power of two.
        assert_eq!(pool_class(5, 6), 6);
        assert_eq!(pool_class(3, 2), 2);
        assert_eq!(pool_class(4, 0), 1);
    }

    #[test]
    fn signature_tracks_model_selection_and_demands() {
        let base = EngramConfig::default();
        let k = provider_cache_key(&base, None, 2);
        assert_eq!(k, provider_cache_key(&base, None, 2));

        // Backend override and pool size are folded in.
        assert_ne!(
            k,
            provider_cache_key(&base, Some(EmbeddingBackend::Onnx), 2)
        );
        assert_ne!(k, provider_cache_key(&base, None, 4));

        // Each model-selecting field changes the signature.
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
        c.rerank.enabled = !c.rerank.enabled;
        assert_ne!(k, provider_cache_key(&c, None, 2));
        assert_eq!(base.title.strategy, crate::title::TitleStrategy::T5);
        let mut c = base.clone();
        c.title.strategy = crate::title::TitleStrategy::Keyword;
        assert_ne!(k, provider_cache_key(&c, None, 2));
        assert_eq!(base.title.pool_size, None);
        let mut c = base.clone();
        c.title.pool_size = Some(3);
        assert_ne!(k, provider_cache_key(&c, None, 2));

        // A model's name matters only while it is selected: a disabled
        // model loads nothing, so its name must not split the cache.
        assert!(!base.nli.enabled);
        let mut c = base.clone();
        c.nli.model = "other-nli".to_string();
        assert_eq!(k, provider_cache_key(&c, None, 2));
        let mut enabled = base.clone();
        enabled.nli.enabled = true;
        let mut c = enabled.clone();
        c.nli.model = "other-nli".to_string();
        assert_ne!(
            provider_cache_key(&enabled, None, 2),
            provider_cache_key(&c, None, 2)
        );
        assert!(base.rerank.enabled);
        let mut c = base.clone();
        c.rerank.model = "other-reranker".to_string();
        assert_ne!(k, provider_cache_key(&c, None, 2));

        // A leftover project `[daemon]` table is never model identity.
        let mut c = base.clone();
        c.legacy_daemon = Some(toml::from_str("idle_timeout_secs = 61").unwrap());
        assert_eq!(k, provider_cache_key(&c, None, 2));
    }

    #[tokio::test]
    async fn starts_empty() {
        assert_eq!(ProviderCache::new().loaded_count().await, 0);
    }

    /// The defect this module exists for: configs that differ in other
    /// models share one embedding model instead of each loading their own.
    #[tokio::test]
    async fn configs_differing_in_other_models_share_the_embedding_model() {
        let (loader, cache) = fake_cache(8);
        let plain = embed_only(2);
        let mut with_title = embed_only(2);
        with_title.title.strategy = crate::title::TitleStrategy::T5;
        let mut with_rerank = embed_only(2);
        with_rerank.rerank.enabled = true;

        let a = cache.get(&plain, None).await;
        let b = cache.get(&with_title, None).await;
        let c = cache.get(&with_rerank, None).await;

        assert_eq!(loader.loads_of(is_embedding), vec![2], "one embedding load");
        assert!(Arc::ptr_eq(
            a.embedding.as_ref().unwrap(),
            b.embedding.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            a.embedding.as_ref().unwrap(),
            c.embedding.as_ref().unwrap()
        ));
        assert!(a.title.is_none() && b.title.is_some());
        assert!(a.reranker.is_none() && c.reranker.is_some());
        assert_eq!(cache.loaded_count().await, 3, "embedding, T5, reranker");
    }

    /// Pool sizes 1, 2 and 3 share one pool, sized to the largest request
    /// rounded up to a power of two. Growing happens in the background: the
    /// request that asks for more is served by the current pool.
    #[tokio::test]
    async fn pools_are_shared_at_the_largest_demand_rounded_up() {
        let (loader, cache) = fake_cache(8);
        assert_eq!(embed_id(&cache, 1).await, "test/fake-x1");
        // Served at once by the current pool, then grown.
        assert_eq!(embed_id(&cache, 2).await, "test/fake-x1");
        eventually(|| embedding_pool(&cache) == Some(2)).await;
        assert_eq!(embed_id(&cache, 3).await, "test/fake-x2");
        eventually(|| embedding_pool(&cache) == Some(4)).await;
        // Every size class at or below the pool is now served by it, with no
        // further load.
        for pool in [1, 2, 3, 4] {
            assert_eq!(embed_id(&cache, pool).await, "test/fake-x4");
        }
        assert_eq!(loader.loads_of(is_embedding), vec![1, 2, 4]);
        assert_eq!(cache.model_ids().await, vec!["embed=test/fake-x4 x4"]);
    }

    /// When the largest demand expires, the pool shrinks to the largest one
    /// left; when every demand expires, the model is unloaded.
    #[tokio::test]
    async fn expired_demands_shrink_then_unload() {
        let (loader, cache) = fake_cache(8);
        let _ = embed_id(&cache, 4).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        // A newer, smaller demand: the 4 is now the older one.
        let _ = embed_id(&cache, 2).await;
        assert_eq!(cache.evict_idle(Duration::from_millis(150)).await, 0);
        eventually(|| loader.loads_of(is_embedding) == vec![4, 2]).await;
        eventually(|| cache.inner.released.load(Ordering::Relaxed) == 1).await;
        assert_eq!(embed_id(&cache, 2).await, "test/fake-x2");
        assert_eq!(cache.take_released(), 1);

        assert_eq!(cache.evict_idle(Duration::ZERO).await, 1);
        assert_eq!(cache.loaded_count().await, 0);
        assert_eq!(cache.take_released(), 1);
    }

    /// A lease (`touch`) renews a demand without loading; a model a caller
    /// still holds survives even a zero window.
    #[tokio::test]
    async fn leases_renew_and_held_models_survive_eviction() {
        let (loader, cache) = fake_cache(8);
        assert!(
            !cache.touch(&embed_only(2), None).await,
            "touch never loads"
        );
        assert_eq!(cache.loaded_count().await, 0);
        assert!(loader.loads_of(|_| true).is_empty());

        let held = cache.get(&embed_only(2), None).await;
        assert_eq!(cache.evict_idle(Duration::ZERO).await, 0, "held");
        drop(held);
        // The zero window above expired the demand; a lease renews it.
        assert!(cache.touch(&embed_only(2), None).await);
        assert_eq!(cache.evict_idle(Duration::from_secs(60)).await, 0);
        assert_eq!(cache.evict_idle(Duration::ZERO).await, 1);
        assert_eq!(cache.loaded_count().await, 0);
    }

    /// A failed load is reported as unavailable for the retry window, then
    /// re-attempted — and one model failing does not take others down.
    #[tokio::test]
    async fn failed_loads_retry_after_their_window_per_model() {
        let (loader, cache) = fake_cache(8);
        loader.fail.store(true, Ordering::SeqCst);
        assert!(cache.get(&embed_only(1), None).await.embedding.is_none());
        loader.fail.store(false, Ordering::SeqCst);
        // Inside the window: not re-attempted.
        assert!(cache.get(&embed_only(1), None).await.embedding.is_none());
        assert_eq!(loader.loads_of(is_embedding).len(), 1);
        // Another model still loads.
        let mut titled = embed_only(1);
        titled.title.strategy = crate::title::TitleStrategy::T5;
        assert!(cache.get(&titled, None).await.title.is_some());

        // Age the failure past the window.
        let slot = cache.slot(&model_demands(&embed_only(1), None, 1)[0].0);
        let Some(old) = Instant::now().checked_sub(FAILED_LOAD_RETRY_AFTER * 2) else {
            return;
        };
        lock(&slot.state).failed_at = Some(old);
        assert!(cache.get(&embed_only(1), None).await.embedding.is_some());
        assert_eq!(loader.loads_of(is_embedding).len(), 2);
    }
}
