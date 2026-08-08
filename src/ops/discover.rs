//! Filesystem discovery of EngramDB projects that the registry doesn't know
//! about.
//!
//! A project is a directory containing a `.engramdb/` directory. The registry
//! (`registry.json`) is the machine-wide index of those projects, but it is
//! only written when a project is `init`'d or opened on *this* machine — so a
//! cloned repo that already carries `.engramdb/memories/`, a project restored
//! from backup, or a registry that was pruned too eagerly all leave real
//! projects invisible to `projects list`, `projects stats`, and every
//! cross-project surface.
//!
//! [`discover_projects`] walks a directory tree and classifies every project it
//! finds against the registry, so a front-end can offer to register (and
//! reindex) the ones that are missing. The walk itself never mutates anything.

use crate::storage::{paths, project_id, Registry, RegistryBackend, RegistryEntry};
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// Default maximum directory depth to descend below the scan root.
///
/// Deep enough to reach the usual `~/src/<org>/<repo>` and monorepo
/// `<repo>/packages/<pkg>` layouts, shallow enough that scanning a home
/// directory stays fast.
pub const DEFAULT_MAX_DEPTH: usize = 6;

/// Directory names never descended into. These are dependency/build/VCS trees
/// that cannot contain a project root but can hold hundreds of thousands of
/// entries, so skipping them is what makes scanning a home directory viable.
pub const DEFAULT_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".engramdb",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".nuxt",
    ".gradle",
    ".terraform",
];

/// Knobs for [`discover_projects`].
#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    /// Maximum depth to descend below the root (root itself is depth 0).
    pub max_depth: usize,
    /// Descend into dot-directories (off by default; `.engramdb/` itself is
    /// always detected regardless — it is the marker, not a scan target).
    pub include_hidden: bool,
    /// Follow directory symlinks. Off by default; the walk is cycle-safe
    /// either way (canonical paths are visited at most once).
    pub follow_symlinks: bool,
    /// Directory names never descended into.
    pub skip_dirs: Vec<String>,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            include_hidden: false,
            follow_symlinks: false,
            skip_dirs: DEFAULT_SKIP_DIRS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// How a discovered project relates to the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryStatus {
    /// Not in the registry — the actionable case.
    Unregistered,
    /// Already tracked: a registry entry points at this exact directory.
    Registered,
    /// This directory's project ID is already registered to a *different*
    /// directory that still exists — two clones of the same git remote hash to
    /// the same ID and would share one index. Registering here would not
    /// repoint the entry (`RegistryBackend::update` keeps the first
    /// registration), so these are reported, never auto-registered.
    SharedId {
        /// The checkout that currently owns the project ID.
        owner: PathBuf,
    },
}

/// A `.engramdb/` project found on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredProject {
    /// Canonicalized project root (the directory *containing* `.engramdb/`).
    pub path: PathBuf,
    /// Project ID this directory would resolve to.
    pub project_id: String,
    /// Number of shared `.md` memory files in `.engramdb/memories/`. Personal
    /// memories live in the global data dir and are not counted here.
    pub memory_count: usize,
    /// Relation to the registry.
    pub status: DiscoveryStatus,
}

/// Everything one scan found.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryReport {
    /// Every project found, sorted by path.
    pub projects: Vec<DiscoveredProject>,
    /// Directories visited (excludes pruned subtrees).
    pub scanned_dirs: usize,
    /// True when at least one subtree was cut off by `max_depth` — the scan
    /// may be incomplete, so front-ends should suggest raising it.
    pub depth_limited: bool,
}

impl DiscoveryReport {
    /// Projects the registry doesn't know about — the ones worth offering.
    pub fn unregistered(&self) -> impl Iterator<Item = &DiscoveredProject> {
        self.projects
            .iter()
            .filter(|p| p.status == DiscoveryStatus::Unregistered)
    }

    /// Projects already tracked by the registry.
    pub fn registered(&self) -> impl Iterator<Item = &DiscoveredProject> {
        self.projects
            .iter()
            .filter(|p| p.status == DiscoveryStatus::Registered)
    }

    /// Projects whose ID is owned by another existing checkout.
    pub fn shared_id(&self) -> impl Iterator<Item = &DiscoveredProject> {
        self.projects
            .iter()
            .filter(|p| matches!(p.status, DiscoveryStatus::SharedId { .. }))
    }
}

/// Walk `root` and classify every `.engramdb/` project found against the
/// registry.
///
/// `on_dir` is invoked once per visited directory (progress reporting); it must
/// be cheap and thread-safe.
pub async fn discover_projects(
    root: &Path,
    registry: &dyn RegistryBackend,
    opts: &DiscoverOptions,
    on_dir: impl Fn(&Path) + Send + Sync,
) -> Result<DiscoveryReport> {
    let reg = registry.load().await?;
    discover_projects_in(root, &reg, opts, on_dir).await
}

/// [`discover_projects`] against an already-loaded registry snapshot.
pub async fn discover_projects_in(
    root: &Path,
    reg: &Registry,
    opts: &DiscoverOptions,
    on_dir: impl Fn(&Path) + Send + Sync,
) -> Result<DiscoveryReport> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }

    // Index the registry once: by canonical path (is *this* directory tracked?)
    // and by project ID (is this ID owned by another checkout?).
    let mut by_path: HashSet<PathBuf> = HashSet::new();
    let mut by_id: HashMap<&str, &RegistryEntry> = HashMap::new();
    for entry in &reg.projects {
        by_path.insert(canonical(Path::new(&entry.project_path)));
        by_id.entry(entry.project_id.as_str()).or_insert(entry);
    }

    // The global/group stores live under the global data dir in the same
    // `.engramdb/` layout. They are engramdb's own storage, not user projects,
    // and are never registry entries — without this guard, scanning a home
    // directory would offer to "register" them.
    let internal_root = paths::global_data_dir().ok().map(|p| canonical(&p));

    let mut report = DiscoveryReport::default();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        let canon = canonical(&dir);
        // Cycle/duplicate guard: a directory reachable twice (symlinks, or a
        // root passed under two spellings) is walked once.
        if !visited.insert(canon.clone()) {
            continue;
        }
        if internal_root
            .as_ref()
            .is_some_and(|internal| canon.starts_with(internal))
        {
            continue;
        }

        report.scanned_dirs += 1;
        on_dir(&dir);

        if is_dir(&canon.join(".engramdb")).await {
            let project_id = project_id::compute_project_id(&canon);
            let status = classify(&canon, &project_id, &by_path, &by_id);
            let memory_count = count_memory_files(&paths::memories_dir(&canon)).await;
            report.projects.push(DiscoveredProject {
                path: canon.clone(),
                project_id,
                memory_count,
                status,
            });
            // Fall through: a project can contain nested projects (monorepo
            // packages), so keep descending — `.engramdb` itself is in
            // `DEFAULT_SKIP_DIRS`.
        }

        let Ok(mut entries) = async_fs::read_dir(&canon).await else {
            // Unreadable directory (permissions): skip it, keep scanning.
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_symlink() && !opts.follow_symlinks {
                continue;
            }
            if !is_dir(&entry.path()).await {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if opts.skip_dirs.iter().any(|s| s == &name) {
                continue;
            }
            if !opts.include_hidden && name.starts_with('.') {
                continue;
            }
            // The depth check lives here, past the filters, so `depth_limited`
            // means "a directory we would have scanned was cut off" — reaching
            // max depth on a leaf (or on one holding nothing but skipped dirs)
            // is a complete scan, not a truncated one.
            if depth >= opts.max_depth {
                report.depth_limited = true;
                continue;
            }
            stack.push((entry.path(), depth + 1));
        }
    }

    report.projects.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(report)
}

/// Classify one project root against the registry indices.
fn classify(
    canon: &Path,
    project_id: &str,
    by_path: &HashSet<PathBuf>,
    by_id: &HashMap<&str, &RegistryEntry>,
) -> DiscoveryStatus {
    if by_path.contains(canon) {
        return DiscoveryStatus::Registered;
    }
    // The path isn't tracked, but the ID might be — two clones of the same git
    // remote share an ID. Only an owner that still exists is a conflict; a
    // vanished one is the moved-project case, which re-registration heals.
    if let Some(entry) = by_id.get(project_id) {
        let owner = canonical(Path::new(&entry.project_path));
        if owner != canon && owner.exists() {
            return DiscoveryStatus::SharedId { owner };
        }
    }
    DiscoveryStatus::Unregistered
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

async fn is_dir(path: &Path) -> bool {
    async_fs::metadata(path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

/// Count `.md` files directly under `dir` (0 when it doesn't exist).
async fn count_memory_files(dir: &Path) -> usize {
    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return 0;
    };
    let mut count = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("md") {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use tempfile::TempDir;

    /// Create a bare project layout (`.engramdb/memories/` + N memory files)
    /// without going through `MemoryStore::init` — i.e. exactly what a fresh
    /// clone of a repo that carries its memories looks like.
    fn fake_project(root: &Path, rel: &str, memories: usize) -> PathBuf {
        let dir = root.join(rel);
        let memories_dir = dir.join(".engramdb").join("memories");
        std::fs::create_dir_all(&memories_dir).unwrap();
        for i in 0..memories {
            std::fs::write(memories_dir.join(format!("mem-{i}.md")), "---\n---\n").unwrap();
        }
        dir
    }

    async fn scan(root: &Path, reg: &Registry) -> DiscoveryReport {
        discover_projects_in(root, reg, &DiscoverOptions::default(), |_| {})
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn finds_unregistered_project_at_root() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), ".", 3);

        let report = scan(tmp.path(), &Registry::default()).await;
        let found: Vec<_> = report.unregistered().collect();
        assert_eq!(found.len(), 1, "the root itself must be scanned");
        assert_eq!(found[0].memory_count, 3);
        assert_eq!(found[0].path, tmp.path().canonicalize().unwrap());
    }

    #[tokio::test]
    async fn finds_nested_projects_and_ignores_plain_dirs() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "a", 1);
        fake_project(tmp.path(), "nested/b", 0);
        std::fs::create_dir_all(tmp.path().join("nested/not-a-project/src")).unwrap();

        let report = scan(tmp.path(), &Registry::default()).await;
        let paths: Vec<_> = report
            .unregistered()
            .map(|p| p.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(paths, vec!["a", "b"], "sorted by path, plain dirs excluded");
    }

    #[tokio::test]
    async fn finds_project_nested_inside_another_project() {
        // Monorepo shape: the outer repo and one of its packages both carry
        // their own `.engramdb/`. Finding the outer one must not stop the walk.
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), ".", 0);
        fake_project(tmp.path(), "packages/inner", 0);

        let report = scan(tmp.path(), &Registry::default()).await;
        assert_eq!(report.unregistered().count(), 2);
    }

    #[tokio::test]
    async fn skips_dependency_and_hidden_directories() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "node_modules/pkg", 0);
        fake_project(tmp.path(), "target/debug/x", 0);
        fake_project(tmp.path(), ".cache/hidden", 0);
        fake_project(tmp.path(), "real", 0);

        let report = scan(tmp.path(), &Registry::default()).await;
        let names: Vec<_> = report
            .unregistered()
            .map(|p| p.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["real"]);
    }

    #[tokio::test]
    async fn include_hidden_opt_in_finds_dot_directories() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), ".hidden/proj", 0);

        let opts = DiscoverOptions {
            include_hidden: true,
            ..Default::default()
        };
        let report = discover_projects_in(tmp.path(), &Registry::default(), &opts, |_| {})
            .await
            .unwrap();
        assert_eq!(report.unregistered().count(), 1);
    }

    #[tokio::test]
    async fn max_depth_bounds_the_walk_and_is_reported() {
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "a/b/c", 0);

        let opts = DiscoverOptions {
            max_depth: 1,
            ..Default::default()
        };
        let report = discover_projects_in(tmp.path(), &Registry::default(), &opts, |_| {})
            .await
            .unwrap();
        assert_eq!(report.unregistered().count(), 0);
        assert!(
            report.depth_limited,
            "a truncated walk must say so, or the empty result reads as 'nothing there'"
        );

        let opts = DiscoverOptions {
            max_depth: 3,
            ..Default::default()
        };
        let report = discover_projects_in(tmp.path(), &Registry::default(), &opts, |_| {})
            .await
            .unwrap();
        assert_eq!(report.unregistered().count(), 1);
        assert!(
            !report.depth_limited,
            "reaching max depth on a leaf is a complete scan, not a truncated one"
        );
    }

    #[tokio::test]
    async fn registered_project_is_classified_not_offered() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let dir = tmp.path().join("tracked");
        std::fs::create_dir_all(&dir).unwrap();
        MemoryStore::init(&dir, &registry).await.unwrap();

        let reg = registry.load().await.unwrap();
        let report = scan(tmp.path(), &reg).await;
        assert_eq!(report.unregistered().count(), 0);
        assert_eq!(report.registered().count(), 1);
    }

    #[tokio::test]
    async fn project_id_owned_by_another_live_checkout_is_shared_not_unregistered() {
        let tmp = TempDir::new().unwrap();
        let owner = fake_project(tmp.path(), "owner", 0);
        let clone = fake_project(tmp.path(), "clone", 0);

        // Both directories claim the same ID (as two clones of one git remote
        // do), and the registry already points at `owner`.
        let shared_id = project_id::compute_project_id(&clone);
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: shared_id.clone(),
            project_path: owner.to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });

        let report = discover_projects_in(&clone, &reg, &DiscoverOptions::default(), |_| {})
            .await
            .unwrap();
        let found: Vec<_> = report.shared_id().collect();
        assert_eq!(found.len(), 1);
        assert!(matches!(
            &found[0].status,
            DiscoveryStatus::SharedId { owner: o } if o == &owner.canonicalize().unwrap()
        ));
        assert_eq!(report.unregistered().count(), 0);
    }

    #[tokio::test]
    async fn stale_registry_entry_for_same_id_does_not_mask_a_real_project() {
        // The registered checkout is gone (moved/re-cloned project): that is
        // the self-healing case, so the surviving directory is offerable.
        let tmp = TempDir::new().unwrap();
        let dir = fake_project(tmp.path(), "here", 0);
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: project_id::compute_project_id(&dir),
            project_path: tmp.path().join("gone").to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });

        let report = discover_projects_in(&dir, &reg, &DiscoverOptions::default(), |_| {})
            .await
            .unwrap();
        assert_eq!(report.unregistered().count(), 1);
    }

    #[tokio::test]
    async fn internal_global_store_is_never_offered() {
        // The global store lives under the global data dir in the same
        // `.engramdb/` layout; scanning it must not surface it as a project.
        let global_root = paths::global_data_dir().unwrap();
        std::fs::create_dir_all(&global_root).unwrap();
        MemoryStore::init_global().await.unwrap();

        let report = scan(&global_root, &Registry::default()).await;
        assert_eq!(
            report.projects.len(),
            0,
            "engramdb's own stores are not user projects"
        );
    }

    #[tokio::test]
    async fn progress_callback_fires_once_per_directory() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let tmp = TempDir::new().unwrap();
        fake_project(tmp.path(), "a", 0);
        std::fs::create_dir_all(tmp.path().join("b/c")).unwrap();

        let seen = AtomicUsize::new(0);
        let report = discover_projects_in(
            tmp.path(),
            &Registry::default(),
            &DiscoverOptions::default(),
            |_| {
                seen.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();
        // root + a + b + b/c (a/.engramdb is a skip_dirs entry).
        assert_eq!(report.scanned_dirs, 4);
        assert_eq!(seen.load(Ordering::Relaxed), report.scanned_dirs);
    }

    #[tokio::test]
    async fn non_directory_root_errors() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(discover_projects_in(
            &file,
            &Registry::default(),
            &DiscoverOptions::default(),
            |_| {}
        )
        .await
        .is_err());
    }
}
