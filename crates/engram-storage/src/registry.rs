//! Registry backend trait and implementations.
//!
//! The registry tracks all EngramDB projects on this machine.  Production code
//! uses [`FileRegistry`] (reads/writes JSON to disk) while tests use
//! [`InMemoryRegistry`] (zero filesystem access, full isolation).

use super::error::Result;
use super::paths;
use super::write_lock;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs as async_fs;
use tokio::sync::Mutex;

/// Entry in the global registry tracking a single project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Unique project identifier (hash of git remote or path)
    pub project_id: String,
    /// Absolute path to the project directory
    pub project_path: String,
    /// If this project is a sub-project (e.g. a git worktree), the project ID
    /// of its parent.  Memory operations on a sub-project are routed to the
    /// root of the hierarchy; the child has no local storage of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_project_id: Option<String>,
}

/// Global registry of all EngramDB projects on this machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// List of all registered projects
    pub projects: Vec<RegistryEntry>,
}

/// Trait for registry persistence backends.
#[async_trait]
pub trait RegistryBackend: Send + Sync {
    /// Load the full registry.
    async fn load(&self) -> Result<Registry>;

    /// Save the full registry.
    async fn save(&self, registry: &Registry) -> Result<()>;

    /// Load, upsert a project entry (without parent), and save.
    ///
    /// This preserves any existing `parent_project_id` on the entry — use
    /// [`update_with_parent`](Self::update_with_parent) if you need to
    /// set/clear the parent explicitly.
    async fn update(&self, dir: &Path, project_id: &str) -> Result<()> {
        self.update_inner(dir, project_id, None, false).await
    }

    /// Load, upsert a project entry with a parent link, and save.
    ///
    /// Passing `parent_project_id = None` explicitly *clears* any existing
    /// parent on the entry (promoting the project to a root).
    async fn update_with_parent(
        &self,
        dir: &Path,
        project_id: &str,
        parent_project_id: Option<&str>,
    ) -> Result<()> {
        self.update_inner(dir, project_id, parent_project_id, true)
            .await
    }

    /// Internal upsert used by both `update` and `update_with_parent`.
    /// When `overwrite_parent` is false, the existing parent on the entry
    /// (if any) is preserved.
    ///
    /// This is the single locking point for upserts: `update` and
    /// `update_with_parent` both delegate here, so a backend that overrides
    /// it with a critical section (see [`FileRegistry`]) takes the lock
    /// exactly once per mutation.
    #[doc(hidden)]
    async fn update_inner(
        &self,
        dir: &Path,
        project_id: &str,
        parent_project_id: Option<&str>,
        overwrite_parent: bool,
    ) -> Result<()> {
        update_inner_impl(self, dir, project_id, parent_project_id, overwrite_parent).await
    }

    /// Set (or clear) the `parent_project_id` of an already-registered project.
    ///
    /// Unlike [`update_with_parent`](Self::update_with_parent), this does not
    /// touch the project path and returns an error if `project_id` is not in
    /// the registry. Pass `parent_project_id = None` to promote the project
    /// back to a root.
    async fn set_parent(&self, project_id: &str, parent_project_id: Option<&str>) -> Result<()> {
        set_parent_impl(self, project_id, parent_project_id).await
    }
}

/// Load → mutate → save body shared by the trait's default `update_inner`
/// and [`FileRegistry`]'s lock-wrapped override. Calls only `load`/`save` on
/// the backend — never another mutating trait method — so a caller already
/// holding the registry lock cannot re-enter it.
async fn update_inner_impl<B>(
    backend: &B,
    dir: &Path,
    project_id: &str,
    parent_project_id: Option<&str>,
    overwrite_parent: bool,
) -> Result<()>
where
    B: RegistryBackend + ?Sized,
{
    let mut registry = backend.load().await?;

    let abs_path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let path_str = abs_path.to_string_lossy().to_string();

    if let Some(entry) = registry
        .projects
        .iter_mut()
        .find(|e| e.project_id == project_id)
    {
        // Keep the first registration. Two independent clones of the same
        // git remote hash to the same project ID (a deliberate feature:
        // the ID is stable across moves and re-clones), so they share one
        // LanceDB index, write lock, and personal-memories dir. The
        // registry's `project_path` records which checkout *owns* the ID;
        // silently repointing it on every open would let the second clone
        // steal ownership. Repoint only when the registered path no
        // longer exists (the legitimate moved/re-cloned self-heal case)
        // or when it is the same directory under a different spelling.
        let registered = PathBuf::from(&entry.project_path);
        let registered_canon = registered
            .canonicalize()
            .unwrap_or_else(|_| registered.clone());
        if registered_canon == abs_path || !registered_canon.exists() {
            entry.project_path = path_str;
        } else {
            tracing::warn!(
                "Registry entry for project '{}' already points at existing checkout {}; \
                 keeping it ({} shares the same project ID)",
                project_id,
                entry.project_path,
                path_str
            );
        }
        if overwrite_parent {
            entry.parent_project_id = parent_project_id.map(|s| s.to_string());
        }
    } else {
        registry.projects.push(RegistryEntry {
            project_id: project_id.to_string(),
            project_path: path_str,
            parent_project_id: parent_project_id.map(|s| s.to_string()),
        });
    }

    backend.save(&registry).await
}

/// Load → mutate → save body shared by the trait's default `set_parent` and
/// [`FileRegistry`]'s lock-wrapped override.
async fn set_parent_impl<B>(
    backend: &B,
    project_id: &str,
    parent_project_id: Option<&str>,
) -> Result<()>
where
    B: RegistryBackend + ?Sized,
{
    let mut registry = backend.load().await?;
    let entry = registry
        .projects
        .iter_mut()
        .find(|e| e.project_id == project_id)
        .ok_or_else(|| {
            super::error::StorageError::Validation(format!(
                "Project '{}' not found in registry",
                project_id
            ))
        })?;
    entry.parent_project_id = parent_project_id.map(|s| s.to_string());
    backend.save(&registry).await
}

/// Collect all direct children of `project_id`.
pub fn list_children<'a>(registry: &'a Registry, project_id: &str) -> Vec<&'a RegistryEntry> {
    registry
        .projects
        .iter()
        .filter(|e| e.parent_project_id.as_deref() == Some(project_id))
        .collect()
}

/// Breadth-first walk of all descendants of `project_id`, returning project
/// ids. Cycle-safe (visited-set bounded by registry size). Does not include
/// `project_id` itself.
pub fn collect_descendants(registry: &Registry, project_id: &str) -> Vec<String> {
    use std::collections::{HashSet, VecDeque};
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Seed with the starting node so a cycle back to it doesn't cause it to
    // be reported as its own descendant.
    seen.insert(project_id.to_string());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(project_id.to_string());

    while let Some(current) = queue.pop_front() {
        for child in list_children(registry, &current) {
            if seen.insert(child.project_id.clone()) {
                out.push(child.project_id.clone());
                queue.push_back(child.project_id.clone());
            }
        }
    }
    out
}

/// Walk `parent_project_id` links in the registry to find the root project.
///
/// Returns `project_id` unchanged if it has no parent or is not found in the
/// registry.  Detects and breaks cycles by bounding the chain length at the
/// number of projects in the registry.
pub fn resolve_root_project_id(registry: &Registry, project_id: &str) -> String {
    let mut current = project_id.to_string();
    // Bound the loop by registry size to guard against cycles.
    for _ in 0..=registry.projects.len() {
        match registry.projects.iter().find(|e| e.project_id == current) {
            Some(entry) => match &entry.parent_project_id {
                Some(parent) if parent != &current => current = parent.clone(),
                _ => return current,
            },
            None => return current,
        }
    }
    current
}

/// Detect whether `current_dir`'s claim on `project_id` conflicts with a
/// different, still-existing checkout recorded in the registry.
///
/// Two independent clones of the same git remote hash to the same project ID
/// and therefore share one machine-global LanceDB index, write lock, and
/// personal-memories dir — while each keeps its own
/// `<clone>/.engramdb/memories/` files. Destructive index operations run from
/// the non-registered clone would silently delete the registered clone's
/// index rows and vectors.
///
/// Returns the registered checkout's canonicalized path when:
/// - the registry entry for `project_id` points at a different directory than
///   `current_dir` (after canonicalization),
/// - that directory still exists (a vanished path is the moved-clone case,
///   which self-heals via re-registration, not a conflict), and
/// - `current_dir` is not a linked git worktree of the registered checkout
///   (worktrees legitimately route to the main checkout's project).
pub fn conflicting_checkout_path(
    registry: &Registry,
    project_id: &str,
    current_dir: &Path,
) -> Option<PathBuf> {
    let entry = registry
        .projects
        .iter()
        .find(|e| e.project_id == project_id)?;
    let registered = PathBuf::from(&entry.project_path);
    let registered_canon = registered
        .canonicalize()
        .unwrap_or_else(|_| registered.clone());
    let current_canon = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    if registered_canon == current_canon || !registered_canon.exists() {
        return None;
    }
    // A linked worktree of the registered checkout shares the main project's
    // storage by design — not a conflict. (Worktrees normally compute their
    // own path-derived ID and never collide here, but stay defensive in case
    // a store is opened at the worktree path directly.)
    if let Some(main) = crate::project_id::detect_worktree_main(current_dir) {
        let main_canon = main.canonicalize().unwrap_or(main);
        if main_canon == registered_canon {
            return None;
        }
    }
    Some(registered_canon)
}

// ---------------------------------------------------------------------------
// FileRegistry — reads/writes JSON to a file on disk
// ---------------------------------------------------------------------------

/// File-backed registry that persists to a JSON file.
///
/// Mutating operations (`update`, `update_with_parent`, `set_parent`) are
/// serialized across processes by an advisory `flock(2)` on a lock file next
/// to the registry (`registry.json.lock`): the whole load → mutate → save
/// cycle runs as one critical section, so two concurrent `engramdb`
/// invocations registering different projects cannot lose each other's
/// entries. Plain reads (`load`) stay lock-free — `atomic_write` keeps the
/// file consistent at all times.
pub struct FileRegistry {
    path: PathBuf,
}

impl FileRegistry {
    /// Create a `FileRegistry` pointing at the platform-default global path.
    pub fn global() -> Result<Self> {
        Ok(Self {
            path: paths::registry_path()?,
        })
    }

    /// Create a `FileRegistry` at an arbitrary path (useful for testing).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Path of the advisory lock file guarding read-modify-write cycles:
    /// `<registry-file>.lock` in the same directory (the registry file itself
    /// can't be the lock — `atomic_write` replaces it by rename, which would
    /// silently detach any flock held on the old inode).
    fn lock_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("registry.json"));
        name.push(".lock");
        self.path.with_file_name(name)
    }
}

#[async_trait]
impl RegistryBackend for FileRegistry {
    async fn load(&self) -> Result<Registry> {
        if self.path.exists() {
            let content = async_fs::read_to_string(&self.path).await?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Registry::default())
        }
    }

    async fn save(&self, registry: &Registry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            async_fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(registry)?;
        super::store::atomic_write(&self.path, &content).await?;
        Ok(())
    }

    // The two overrides below wrap the shared load → mutate → save bodies in
    // the cross-process registry lock. They must never call another mutating
    // trait method while the guard is held: `flock` on a fresh fd in the same
    // process blocks just like another process, so re-acquiring would
    // deadlock. (`update`/`update_with_parent` are safe — they delegate to
    // `update_inner` and take the lock exactly once.)

    async fn update_inner(
        &self,
        dir: &Path,
        project_id: &str,
        parent_project_id: Option<&str>,
        overwrite_parent: bool,
    ) -> Result<()> {
        let _lock = write_lock::acquire_lock_file(self.lock_path()).await?;
        update_inner_impl(self, dir, project_id, parent_project_id, overwrite_parent).await
    }

    async fn set_parent(&self, project_id: &str, parent_project_id: Option<&str>) -> Result<()> {
        let _lock = write_lock::acquire_lock_file(self.lock_path()).await?;
        set_parent_impl(self, project_id, parent_project_id).await
    }
}

// ---------------------------------------------------------------------------
// InMemoryRegistry — zero filesystem, fully isolated
// ---------------------------------------------------------------------------

/// In-memory registry for tests.  Zero filesystem access, fully isolated.
pub struct InMemoryRegistry {
    data: Arc<Mutex<Registry>>,
}

impl InMemoryRegistry {
    /// Create an empty in-memory registry.
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(Registry::default())),
        }
    }

    /// Create an in-memory registry pre-populated with the given data.
    pub fn with(registry: Registry) -> Self {
        Self {
            data: Arc::new(Mutex::new(registry)),
        }
    }
}

impl Default for InMemoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryBackend for InMemoryRegistry {
    async fn load(&self) -> Result<Registry> {
        Ok(self.data.lock().await.clone())
    }

    async fn save(&self, registry: &Registry) -> Result<()> {
        *self.data.lock().await = registry.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_file_registry_load_empty() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        let registry = file_registry.load().await.unwrap();
        assert!(registry.projects.is_empty());
    }

    #[tokio::test]
    async fn test_file_registry_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        let mut registry = Registry::default();
        registry.projects.push(RegistryEntry {
            project_id: "test-id".to_string(),
            project_path: "/tmp/test".to_string(),
            parent_project_id: None,
        });

        file_registry.save(&registry).await.unwrap();

        let loaded = file_registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "test-id");
    }

    #[tokio::test]
    async fn test_file_registry_update_creates_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        file_registry
            .update(temp_dir.path(), "proj-1")
            .await
            .unwrap();

        let loaded = file_registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "proj-1");
    }

    #[tokio::test]
    async fn test_file_registry_update_deduplicates() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        file_registry
            .update(temp_dir.path(), "proj-1")
            .await
            .unwrap();
        file_registry
            .update(temp_dir.path(), "proj-1")
            .await
            .unwrap();

        let loaded = file_registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    // ---- cross-process-style concurrency (registry lock) ----

    /// N concurrent registrations of N distinct projects must all survive.
    /// Each task uses its own `FileRegistry` against the same file and the
    /// lock is taken on a fresh fd per acquisition, so in-process tasks
    /// contend on the flock exactly like separate processes would. Without
    /// the lock the load → mutate → save cycles interleave and entries
    /// vanish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_file_registry_concurrent_updates_keep_all_entries() {
        const N: usize = 16;
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");

        let mut handles = Vec::new();
        for i in 0..N {
            let path = registry_path.clone();
            let dir = temp_dir.path().join(format!("proj-{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            handles.push(tokio::spawn(async move {
                FileRegistry::new(path)
                    .update(&dir, &format!("proj-{i}"))
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let loaded = FileRegistry::new(registry_path).load().await.unwrap();
        let mut ids: Vec<_> = loaded
            .projects
            .iter()
            .map(|e| e.project_id.clone())
            .collect();
        ids.sort();
        assert_eq!(
            loaded.projects.len(),
            N,
            "every concurrent registration must survive; got {ids:?}"
        );
    }

    /// Same property across real OS threads (each with its own runtime),
    /// the closest in-process approximation of N separate `engramdb`
    /// processes hitting the registry at once.
    #[test]
    fn test_file_registry_cross_thread_updates_keep_all_entries() {
        const N: usize = 8;
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");

        let mut handles = Vec::new();
        for i in 0..N {
            let path = registry_path.clone();
            let dir = temp_dir.path().join(format!("thread-proj-{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            handles.push(std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    FileRegistry::new(path)
                        .update(&dir, &format!("thread-proj-{i}"))
                        .await
                        .unwrap();
                });
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let loaded = rt
            .block_on(FileRegistry::new(registry_path).load())
            .unwrap();
        assert_eq!(loaded.projects.len(), N);
    }

    /// A worktree parent link set while other registrations race must
    /// survive: without the lock, a concurrent `update`'s stale load can
    /// save right over the freshly written `parent_project_id`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_file_registry_concurrent_parent_link_survives() {
        const N: usize = 8;
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");

        // Register the child up front so set_parent finds it.
        let child_dir = temp_dir.path().join("child");
        std::fs::create_dir_all(&child_dir).unwrap();
        FileRegistry::new(registry_path.clone())
            .update(&child_dir, "child-id")
            .await
            .unwrap();

        let mut handles = Vec::new();
        // One task links the child to its parent...
        {
            let path = registry_path.clone();
            handles.push(tokio::spawn(async move {
                FileRegistry::new(path)
                    .set_parent("child-id", Some("parent-id"))
                    .await
                    .unwrap();
            }));
        }
        // ...while N others register unrelated projects.
        for i in 0..N {
            let path = registry_path.clone();
            let dir = temp_dir.path().join(format!("other-{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            handles.push(tokio::spawn(async move {
                FileRegistry::new(path)
                    .update(&dir, &format!("other-{i}"))
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let loaded = FileRegistry::new(registry_path).load().await.unwrap();
        assert_eq!(loaded.projects.len(), N + 1);
        let child = loaded
            .projects
            .iter()
            .find(|e| e.project_id == "child-id")
            .expect("child entry must survive");
        assert_eq!(
            child.parent_project_id.as_deref(),
            Some("parent-id"),
            "the parent link must not be lost to a concurrent registration"
        );
    }

    #[tokio::test]
    async fn test_in_memory_registry_load_empty() {
        let registry = InMemoryRegistry::new();
        let loaded = registry.load().await.unwrap();
        assert!(loaded.projects.is_empty());
    }

    #[tokio::test]
    async fn test_in_memory_registry_save_and_load() {
        let registry = InMemoryRegistry::new();

        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "mem-id".to_string(),
            project_path: "/tmp/mem".to_string(),
            parent_project_id: None,
        });

        registry.save(&data).await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "mem-id");
    }

    #[tokio::test]
    async fn test_in_memory_registry_update_creates_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(temp_dir.path(), "proj-2").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "proj-2");
    }

    #[tokio::test]
    async fn test_in_memory_registry_update_deduplicates() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(temp_dir.path(), "proj-2").await.unwrap();
        registry.update(temp_dir.path(), "proj-2").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    #[tokio::test]
    async fn test_in_memory_with_preloaded() {
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "pre-1".to_string(),
            project_path: "/tmp/pre".to_string(),
            parent_project_id: None,
        });
        data.projects.push(RegistryEntry {
            project_id: "pre-2".to_string(),
            project_path: "/tmp/pre2".to_string(),
            parent_project_id: None,
        });

        let registry = InMemoryRegistry::with(data);
        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 2);
        assert_eq!(loaded.projects[0].project_id, "pre-1");
        assert_eq!(loaded.projects[1].project_id, "pre-2");
    }

    #[tokio::test]
    async fn test_update_with_parent_sets_parent_id() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry
            .update_with_parent(temp_dir.path(), "child-id", Some("parent-id"))
            .await
            .unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        let entry = &loaded.projects[0];
        assert_eq!(entry.project_id, "child-id");
        assert_eq!(entry.parent_project_id.as_deref(), Some("parent-id"));
    }

    #[tokio::test]
    async fn test_update_preserves_existing_parent() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        // Create a child entry with a parent.
        registry
            .update_with_parent(temp_dir.path(), "child-id", Some("parent-id"))
            .await
            .unwrap();

        // Calling plain `update` must not wipe the parent.
        registry.update(temp_dir.path(), "child-id").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(
            loaded.projects[0].parent_project_id.as_deref(),
            Some("parent-id")
        );
    }

    #[tokio::test]
    async fn test_update_with_parent_none_clears_parent() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry
            .update_with_parent(temp_dir.path(), "child-id", Some("parent-id"))
            .await
            .unwrap();
        registry
            .update_with_parent(temp_dir.path(), "child-id", None)
            .await
            .unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects[0].parent_project_id, None);
    }

    #[test]
    fn test_resolve_root_project_id_no_parent() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "solo".into(),
            project_path: "/tmp/solo".into(),
            parent_project_id: None,
        });
        assert_eq!(resolve_root_project_id(&reg, "solo"), "solo");
    }

    #[test]
    fn test_resolve_root_project_id_single_level() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "root".into(),
            project_path: "/tmp/root".into(),
            parent_project_id: None,
        });
        reg.projects.push(RegistryEntry {
            project_id: "child".into(),
            project_path: "/tmp/child".into(),
            parent_project_id: Some("root".into()),
        });
        assert_eq!(resolve_root_project_id(&reg, "child"), "root");
    }

    #[test]
    fn test_resolve_root_project_id_follows_chain() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: None,
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("a".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "c".into(),
            project_path: "/c".into(),
            parent_project_id: Some("b".into()),
        });
        assert_eq!(resolve_root_project_id(&reg, "c"), "a");
    }

    #[test]
    fn test_resolve_root_project_id_breaks_cycles() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: Some("b".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("a".into()),
        });
        // Must terminate even with a cycle; return value can be either node.
        let root = resolve_root_project_id(&reg, "a");
        assert!(root == "a" || root == "b");
    }

    #[test]
    fn test_resolve_root_project_id_unknown_returns_input() {
        let reg = Registry::default();
        assert_eq!(resolve_root_project_id(&reg, "unknown"), "unknown");
    }

    // ---- registration ownership (second-clone guard) ----

    #[tokio::test]
    async fn test_update_keeps_registered_path_while_it_exists() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(dir_a.path(), "shared-id").await.unwrap();
        // A second checkout claiming the same project ID must NOT steal the
        // registration while the first checkout still exists.
        registry.update(dir_b.path(), "shared-id").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(
            PathBuf::from(&loaded.projects[0].project_path),
            dir_a.path().canonicalize().unwrap(),
            "registration must stay with the first checkout while it exists"
        );
    }

    #[tokio::test]
    async fn test_update_repoints_when_registered_path_gone() {
        let root = TempDir::new().unwrap();
        let dir_a = root.path().join("a");
        let dir_b = root.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(&dir_a, "shared-id").await.unwrap();
        // The registered checkout vanishes (moved / re-cloned project) —
        // the next open from the new location self-heals the entry.
        std::fs::remove_dir_all(&dir_a).unwrap();
        registry.update(&dir_b, "shared-id").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(
            PathBuf::from(&loaded.projects[0].project_path),
            dir_b.canonicalize().unwrap(),
            "a vanished checkout must repoint to the new location"
        );
    }

    #[tokio::test]
    async fn test_update_with_parent_sets_parent_without_repointing() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(dir_a.path(), "shared-id").await.unwrap();
        registry
            .update_with_parent(dir_b.path(), "shared-id", Some("parent-id"))
            .await
            .unwrap();

        let loaded = registry.load().await.unwrap();
        let entry = &loaded.projects[0];
        assert_eq!(
            PathBuf::from(&entry.project_path),
            dir_a.path().canonicalize().unwrap(),
            "path must not be stolen even when setting a parent"
        );
        assert_eq!(entry.parent_project_id.as_deref(), Some("parent-id"));
    }

    // ---- conflicting_checkout_path ----

    fn registry_with_entry(project_id: &str, path: &Path) -> Registry {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: project_id.to_string(),
            project_path: path.to_string_lossy().to_string(),
            parent_project_id: None,
        });
        reg
    }

    #[test]
    fn test_conflicting_checkout_path_detects_second_clone() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let reg = registry_with_entry("shared-id", dir_a.path());

        assert_eq!(
            conflicting_checkout_path(&reg, "shared-id", dir_b.path()),
            Some(dir_a.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn test_conflicting_checkout_path_none_for_registered_owner() {
        let dir_a = TempDir::new().unwrap();
        let reg = registry_with_entry("shared-id", dir_a.path());
        assert_eq!(
            conflicting_checkout_path(&reg, "shared-id", dir_a.path()),
            None
        );
    }

    #[test]
    fn test_conflicting_checkout_path_none_when_registered_path_gone() {
        let root = TempDir::new().unwrap();
        let dir_a = root.path().join("gone");
        std::fs::create_dir_all(&dir_a).unwrap();
        let reg = registry_with_entry("shared-id", &dir_a);
        std::fs::remove_dir_all(&dir_a).unwrap();

        let dir_b = root.path().join("b");
        std::fs::create_dir_all(&dir_b).unwrap();
        assert_eq!(
            conflicting_checkout_path(&reg, "shared-id", &dir_b),
            None,
            "a vanished registered path is the moved-clone case, not a conflict"
        );
    }

    #[test]
    fn test_conflicting_checkout_path_none_for_unknown_project() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            conflicting_checkout_path(&Registry::default(), "no-such-id", dir.path()),
            None
        );
    }

    #[test]
    fn test_conflicting_checkout_path_none_for_linked_worktree_of_registered() {
        // Fake main + linked-worktree layout mirroring git's structure (same
        // shape as the worktree.rs tests).
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        let wt = tmp.path().join("wt");
        let wt_gitdir = main.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        // Registry says the main checkout owns the project ID; opening from
        // the linked worktree legitimately shares that storage.
        let reg = registry_with_entry("shared-id", &main.canonicalize().unwrap());
        assert_eq!(
            conflicting_checkout_path(&reg, "shared-id", &wt),
            None,
            "a linked worktree of the registered checkout is not a conflict"
        );
    }

    #[test]
    fn test_registry_entry_missing_parent_field_deserializes_as_none() {
        // Ensure backward-compat with older registry.json files that don't
        // include parent_project_id.
        let json = r#"{
            "projects": [
                {"project_id": "x", "project_path": "/x"}
            ]
        }"#;
        let reg: Registry = serde_json::from_str(json).unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.projects[0].parent_project_id, None);
    }

    #[tokio::test]
    async fn test_file_registry_load_corrupted_json_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");

        // Write corrupted JSON
        async_fs::write(&registry_path, "{ not valid json !!!")
            .await
            .unwrap();

        let file_registry = FileRegistry::new(registry_path);
        let result = file_registry.load().await;
        assert!(
            result.is_err(),
            "Corrupted registry JSON should return an error, not silently discard data"
        );
    }

    // ---- set_parent ----

    #[tokio::test]
    async fn test_set_parent_on_existing_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        registry.update(temp_dir.path(), "child").await.unwrap();

        registry.set_parent("child", Some("parent")).await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(
            loaded.projects[0].parent_project_id.as_deref(),
            Some("parent")
        );
    }

    #[tokio::test]
    async fn test_set_parent_clears_with_none() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        registry
            .update_with_parent(temp_dir.path(), "child", Some("parent"))
            .await
            .unwrap();

        registry.set_parent("child", None).await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects[0].parent_project_id, None);
    }

    #[tokio::test]
    async fn test_set_parent_errors_when_child_missing() {
        let registry = InMemoryRegistry::new();
        let err = registry
            .set_parent("nonexistent", Some("x"))
            .await
            .expect_err("set_parent on missing child should error");
        assert!(format!("{err}").contains("nonexistent"));
    }

    // ---- list_children / collect_descendants ----

    fn tree_registry() -> Registry {
        // root
        // ├── a
        // │   └── a1
        // └── b
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "root".into(),
            project_path: "/root".into(),
            parent_project_id: None,
        });
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: Some("root".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "a1".into(),
            project_path: "/a1".into(),
            parent_project_id: Some("a".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("root".into()),
        });
        reg
    }

    #[test]
    fn test_list_children_direct_only() {
        let reg = tree_registry();
        let ids: Vec<_> = list_children(&reg, "root")
            .iter()
            .map(|e| e.project_id.clone())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn test_list_children_returns_empty_for_leaf() {
        let reg = tree_registry();
        assert!(list_children(&reg, "b").is_empty());
    }

    #[test]
    fn test_collect_descendants_walks_whole_subtree() {
        let reg = tree_registry();
        let mut desc = collect_descendants(&reg, "root");
        desc.sort();
        assert_eq!(desc, vec!["a", "a1", "b"]);
    }

    #[test]
    fn test_collect_descendants_returns_empty_for_leaf() {
        let reg = tree_registry();
        assert!(collect_descendants(&reg, "a1").is_empty());
    }

    #[test]
    fn test_collect_descendants_cycle_safe() {
        // a → b → a cycle. collect_descendants("a") must terminate.
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: Some("b".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("a".into()),
        });
        let mut desc = collect_descendants(&reg, "a");
        desc.sort();
        // Descendants of `a` includes `b` (whose parent is `a`). `a` itself
        // is not reported. The walk must not loop forever.
        assert_eq!(desc, vec!["b"]);
    }
}
