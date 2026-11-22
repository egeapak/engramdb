//! Running hooks in the daemon: warm stores, the seed cache, the injected
//! executor, and the client side that offers a hook to a running daemon.

use super::*;

/// Built situation classifiers, kept by the daemon so a prompt embeds only
/// itself instead of the 24 seeds and itself.
///
/// Keyed by what the seed vectors depend on: the model id, its width and how
/// many seeds per class. The seeds are constants of this binary, and a new
/// build evicts the daemon, so an entry cannot go stale. At most
/// [`SEED_CACHE_MAX`] entries (one per embedding model the daemon serves);
/// a failed build is not cached. Declared risk: a seed embedding the runtime
/// mis-executed (the AVX-512 prebuilt incident) would now persist for the
/// daemon's life instead of one prompt; the release runtime does not do that.
#[derive(Default)]
pub struct SeedCache {
    entries: std::sync::Mutex<
        std::collections::HashMap<
            (String, usize, usize),
            std::sync::Arc<engramdb::prompt_intent::PrototypeClassifier>,
        >,
    >,
}

/// Bound on [`SeedCache`] entries.
pub(super) const SEED_CACHE_MAX: usize = 4;

impl SeedCache {
    /// The cached classifier for `provider`, building (and caching) it on
    /// first use. `None` when the build fails; the caller then classifies
    /// with [`SituationModel::Provider`] as before.
    pub(super) async fn classifier_for(
        &self,
        provider: &dyn engramdb::embeddings::EmbeddingProvider,
    ) -> Option<std::sync::Arc<engramdb::prompt_intent::PrototypeClassifier>> {
        use engramdb::prompt_intent::{PrototypeClassifier, PrototypeParams};
        let params = PrototypeParams::default();
        let key = (
            provider.model_id(),
            provider.dimensions(),
            params.seeds_per_class,
        );
        if let Some(c) = self.lock().get(&key) {
            return Some(std::sync::Arc::clone(c));
        }
        // Built outside the lock: an embed must not block other prompts.
        // Two concurrent first prompts may both build; the second insert
        // wins, both results are equal.
        let built = match PrototypeClassifier::build(provider, params).await {
            Ok(c) => std::sync::Arc::new(c),
            Err(e) => {
                tracing::debug!("situation classifier seeds failed to embed (non-fatal): {e}");
                return None;
            }
        };
        let mut entries = self.lock();
        if entries.len() >= SEED_CACHE_MAX && !entries.contains_key(&key) {
            if let Some(victim) = entries.keys().next().cloned() {
                entries.remove(&victim);
            }
        }
        entries.insert(key, std::sync::Arc::clone(&built));
        Some(built)
    }

    pub(super) fn lock(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        std::collections::HashMap<
            (String, usize, usize),
            std::sync::Arc<engramdb::prompt_intent::PrototypeClassifier>,
        >,
    > {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The situation model for `provider`: its cached classifier when one
    /// builds, else the provider alone.
    pub(super) async fn model_for(
        &self,
        provider: std::sync::Arc<dyn engramdb::embeddings::EmbeddingProvider>,
    ) -> SituationModel {
        match self.classifier_for(provider.as_ref()).await {
            Some(classifier) => SituationModel::Cached {
                provider,
                classifier,
            },
            None => SituationModel::Provider(provider),
        }
    }
}

/// Where a hook handler gets its store.
pub enum StoreSource<'a> {
    /// Open the store for this call (a hook running in its own process).
    Fresh,
    /// Reuse a warm store (the daemon).
    Cached(&'a StoreCache),
}

impl StoreSource<'_> {
    pub(super) async fn open(&self, dir: &Path) -> engramdb::storage::Result<MemoryStore> {
        match self {
            Self::Fresh => MemoryStore::open(dir).await,
            Self::Cached(cache) => cache.get_or_open(dir).await,
        }
    }
}

/// At most this many projects keep a warm store in the daemon.
const STORE_CACHE_MAX: usize = 16;

/// A project whose store has not been used for this long is dropped.
const STORE_CACHE_IDLE: std::time::Duration = std::time::Duration::from_secs(600);

/// Warm stores, one per project root, kept by the daemon across hook calls.
///
/// Freshness does not depend on this cache: every LanceDB operation reopens
/// its table and so reads the latest version, and memory files are read from
/// disk on each query. What the cache keeps is the open connection and
/// LanceDB's metadata caches. An entry is reopened when `config.toml` or
/// `manifest.toml` changes (the store reads its dimensions from them at
/// open), and refused when `.engramdb/` is gone.
#[derive(Default)]
pub struct StoreCache {
    inner: tokio::sync::Mutex<std::collections::HashMap<std::path::PathBuf, CachedStore>>,
}

struct CachedStore {
    store: MemoryStore,
    stamp: StoreStamp,
    last_used: std::time::Instant,
}

/// Modification times of the files a store reads at open.
type StoreStamp = (Option<std::time::SystemTime>, Option<std::time::SystemTime>);

pub(super) fn store_stamp(dir: &Path) -> StoreStamp {
    let engramdb = engramdb::storage::paths::project_dir(dir);
    let mtime = |name: &str| {
        std::fs::metadata(engramdb.join(name))
            .and_then(|m| m.modified())
            .ok()
    };
    (mtime("config.toml"), mtime("manifest.toml"))
}

impl StoreCache {
    pub(super) async fn get_or_open(&self, dir: &Path) -> engramdb::storage::Result<MemoryStore> {
        let key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let stamp = store_stamp(&key);
        {
            let mut inner = self.inner.lock().await;
            if !engramdb::storage::paths::project_dir(&key).is_dir() {
                inner.remove(&key);
                return Err(engramdb::storage::StorageError::NotInitialized);
            }
            let now = std::time::Instant::now();
            inner.retain(|_, c| now.duration_since(c.last_used) < STORE_CACHE_IDLE);
            if let Some(cached) = inner.get_mut(&key) {
                if cached.stamp == stamp {
                    cached.last_used = now;
                    return Ok(cached.store.clone());
                }
            }
        }
        // Opened without the lock: an open can run a schema migration, and
        // that must not stall every other project's hooks. Two hooks that
        // miss at once both open; the second insert wins, which is harmless.
        let store = MemoryStore::open(dir).await?;
        let now = std::time::Instant::now();
        let mut inner = self.inner.lock().await;
        inner.insert(
            key,
            CachedStore {
                store: store.clone(),
                stamp,
                last_used: now,
            },
        );
        while inner.len() > STORE_CACHE_MAX {
            let Some(oldest) = inner
                .iter()
                .min_by_key(|(_, c)| c.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            inner.remove(&oldest);
        }
        Ok(store)
    }

    /// Number of warm stores.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// Whether no store is warm.
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }
}

/// The hook runner `engramdb daemon run` injects into the daemon.
///
/// Mirrors what the hook's own process does before dispatch — worktree
/// routing (failing open to the given directory, like `cli::run` does for
/// hooks) and the project config — then runs [`execute_hook`] against warm
/// stores, with the daemon's own embedding model for the situation
/// classifier.
pub struct DaemonHookExecutor {
    pub(super) stores: StoreCache,
    registry: engramdb::storage::FileRegistry,
    seeds: SeedCache,
}

impl DaemonHookExecutor {
    pub fn new() -> Result<Self> {
        Ok(Self {
            stores: StoreCache::default(),
            registry: engramdb::storage::FileRegistry::global()?,
            seeds: SeedCache::default(),
        })
    }

    async fn run_inner(
        &self,
        call: &HookCall,
        dir: &Path,
        stdin: &str,
        models: &engramdb::ops::ProviderCache,
    ) -> engramdb::daemon::HookRun {
        let root = match engramdb::storage::worktree::resolve_project_root(dir, &self.registry)
            .await
        {
            Ok(root) => root,
            Err(e) => {
                tracing::warn!(
                    "engramdb hook (daemon): worktree resolution failed (continuing unresolved): {e}"
                );
                dir.to_path_buf()
            }
        };
        let config_path = root.join(".engramdb").join("config.toml");
        let config = engramdb::storage::config::load_config_or_default(&config_path).await;
        let engramdb_dir = engramdb::storage::paths::project_dir(&root);
        let initialized =
            engramdb_dir.join("manifest.toml").is_file() || engramdb_dir.join("memories").is_dir();
        let situation = daemon_situation(call, &config, initialized, models, &self.seeds).await;
        let usage_events = std::sync::Mutex::new(Vec::new());
        let env = HookEnv {
            stores: StoreSource::Cached(&self.stores),
            situation,
            usage: UsageSink::Collect(&usage_events),
            registry: Some(&self.registry),
        };
        let (stdout, ok) = match execute_hook(call, &root, stdin, &env).await {
            Ok(out) => (out, true),
            Err(e) => {
                tracing::warn!("engramdb hook (daemon) failed (continuing without context): {e}");
                (None, false)
            }
        };
        let project = initialized.then(|| engramdb::daemon::HookProject {
            project_id: engramdb::storage::project_id::compute_project_id(&root),
            root: root.clone(),
            stats_enabled: config.stats.enabled,
            retention_days: config.stats.retention_days,
            session_id: extract_session_id(stdin),
        });
        engramdb::daemon::HookRun {
            stdout,
            ok,
            project,
            usage_events: usage_events.into_inner().unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// The situation model for a hook the daemon serves.
///
/// Only UserPromptSubmit uses one, and only in an initialized project (the
/// hook returns nothing elsewhere, so no model is touched there). The
/// embedding model is used only when the daemon already has it loaded; on a
/// cold daemon this prompt classifies by keywords and a background load
/// (pool of one, only when the model can load locally) readies the next one.
/// A hook never waits on a model load and never grows a pool.
async fn daemon_situation(
    call: &HookCall,
    config: &engramdb::types::EngramConfig,
    initialized: bool,
    models: &engramdb::ops::ProviderCache,
    seeds: &SeedCache,
) -> SituationModel {
    if !(matches!(call, HookCall::UserPromptSubmit)
        && config.hooks.situation_classifier
        && initialized)
    {
        return SituationModel::KeywordOnly;
    }
    let backend = engramdb::ops::resolve_backend(config.embeddings.backend, None);
    let warm = engramdb::ops::onnx_embedding_cached(config, backend);
    match models.embedding_for_hook(config, backend, warm).await {
        Some(p) => seeds.model_for(p).await,
        None => SituationModel::KeywordOnly,
    }
}

impl engramdb::daemon::HookExecutor for DaemonHookExecutor {
    fn run<'a>(
        &'a self,
        call: &'a HookCall,
        dir: &'a Path,
        stdin: &'a str,
        models: &'a engramdb::ops::ProviderCache,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = engramdb::daemon::HookRun> + Send + 'a>>
    {
        // Owned copies: borrowing the arguments straight into `run_inner`
        // trips rustc's higher-ranked `Send` check on the nested futures.
        let call = call.clone();
        let dir = dir.to_path_buf();
        let stdin = stdin.to_string();
        Box::pin(async move { self.run_inner(&call, &dir, &stdin, models).await })
    }
}

/// What happened to a hook offered to the daemon.
pub enum Delegated {
    /// The daemon ran it and its output is printed. Nothing else to do: the
    /// daemon records the call in hook stats.
    Done,
    /// The daemon did not run it. Run it in this process.
    RunHere,
}

/// Offer a hook to an already-running daemon, before this process does any
/// project work of its own (registry, worktree routing, config). Never spawns
/// a daemon.
///
/// Falls back ([`Delegated::RunHere`]) only when the daemon provably did not
/// run the hook. When the request was sent and the reply was lost, the hook
/// prints nothing and is not retried here: the daemon may have run it, and a
/// second run could repeat its side effects (the "already shown" record, the
/// SessionEnd archive). That call is also missing from hook stats unless the
/// daemon recorded it — a declared loss.
pub async fn delegate_to_daemon(call: &HookCall, dir: &Path, input: &str) -> Delegated {
    let global = engramdb::storage::config::load_global_config_or_default().await;
    // A hook is a CLI invocation: `use_for_cli = false` ("the CLI never
    // contacts a daemon") keeps it in-process, like every other command.
    if !global.daemon.enabled || !global.daemon.use_for_cli {
        return Delegated::RunHere;
    }
    let socket = engramdb::daemon::resolve_socket(None, &global.daemon);
    // The daemon's working directory is unrelated to ours.
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    match engramdb::daemon::delegate_hook(&socket, call, &dir, input).await {
        engramdb::daemon::HookDelegation::Served { stdout, .. } => {
            print_hook_output(stdout.as_deref());
            Delegated::Done
        }
        engramdb::daemon::HookDelegation::NotSent(why) => {
            tracing::debug!(
                "engramdb hook: daemon did not take `{}` ({why})",
                call.name()
            );
            Delegated::RunHere
        }
        engramdb::daemon::HookDelegation::Lost(why) => {
            tracing::warn!(
                "engramdb hook: lost the daemon's reply for `{}` ({why}); continuing without context",
                call.name()
            );
            Delegated::Done
        }
    }
}
