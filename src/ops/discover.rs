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
    /// This directory's project ID is claimed by a *different* directory that
    /// still exists — either an existing registry entry, or an earlier
    /// candidate in this same scan. Two clones of one git remote hash to the
    /// same ID and would share a single index, and registering the second
    /// would not repoint the entry (`RegistryBackend::update` keeps the first
    /// registration), so these are reported, never auto-registered.
    SharedId {
        /// The checkout that owns (or will own) the project ID.
        owner: PathBuf,
    },
    /// The path IS registered, but under a project ID it no longer hashes to.
    ///
    /// `compute_project_id` prefers the git remote and falls back to the path,
    /// so adding a remote after `engramdb init` silently re-keys the project.
    /// Its memories then disappear from queries (the live ID's index is empty)
    /// and its group subscriptions detach (they are recorded against the old
    /// ID). Adopting is the wrong repair — registering the live ID leaves two
    /// entries for one path — so this is reported and pointed at
    /// `engramdb projects repair`.
    StaleRegistration {
        /// The project ID the registry still records for this path.
        registered_id: String,
    },
    /// A linked git worktree carrying its own `.engramdb/`.
    ///
    /// Worktrees are not independent projects: `worktree::resolve_project_root`
    /// routes their memory operations to the main checkout, consolidates any
    /// stray local store into it, and registers them as *sub-projects* with a
    /// `parent_project_id`. Adopting one as a root project here would create a
    /// second owner of the same memory files — double-counted by
    /// `projects list`/`stats` until the next ordinary command inside the
    /// worktree undoes it. Reported, never auto-registered.
    Worktree {
        /// The main worktree's project root.
        main: PathBuf,
    },
}

/// A `.engramdb/` project found on disk.
#[derive(Debug, Clone)]
pub struct DiscoveredProject {
    /// Canonicalized project root (the directory *containing* `.engramdb/`).
    pub path: PathBuf,
    /// Project ID this directory would resolve to.
    pub project_id: String,
    /// Memory files this project would index: the shared `.md` files under
    /// `.engramdb/memories/` plus the machine-local personal ones. Both are
    /// what a reindex walks, so this is the number the adoption prompt can
    /// honestly show.
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
    /// Directories that could not be listed (permissions, dead mounts). Their
    /// subtrees were skipped, so a nonzero count means "no projects found" is
    /// not the same as "there are none" — front-ends must say so.
    pub unreadable_dirs: usize,
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

    /// Projects found but deliberately not offered: an ID owned by another
    /// checkout, a linked git worktree, or a re-keyed registration. Front-ends
    /// must account for these — silently dropping them makes "found N,
    /// registered fewer" unreconcilable.
    pub fn skipped(&self) -> impl Iterator<Item = &DiscoveredProject> {
        self.projects.iter().filter(|p| {
            matches!(
                p.status,
                DiscoveryStatus::SharedId { .. }
                    | DiscoveryStatus::Worktree { .. }
                    | DiscoveryStatus::StaleRegistration { .. }
            )
        })
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
    let mut by_id: HashMap<String, &RegistryEntry> = HashMap::new();
    for entry in &reg.projects {
        let entry_path = canonical(Path::new(&entry.project_path));
        by_path.insert(entry_path.clone());
        by_id.entry(entry.project_id.clone()).or_insert(entry);
        // Also index the ID the registered path hashes to TODAY. A drifted
        // project owns its live ID even though no row records it, and without
        // this a clone of that project matches nothing — so it is offered for
        // adoption, and its reindex clears the memories table the drifted
        // project is still using.
        // `try_exists`, and an error counts as present: `exists()` collapses
        // EACCES, ESTALE and a hung network mount into "gone", and the failure
        // mode here is adopting (and reindexing) a live project's store.
        if entry_path.try_exists().unwrap_or(true) {
            by_id
                .entry(project_id::compute_project_id(&entry_path))
                .or_insert(entry);
        }
    }

    // The global/group stores live under the global data dir in the same
    // `.engramdb/` layout. They are engramdb's own storage, not user projects,
    // and are never registry entries — without this guard, scanning a home
    // directory would offer to "register" them.
    let internal_root = paths::global_data_dir().ok().map(|p| canonical(&p));

    let mut report = DiscoveryReport::default();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    // Paths on the stack are always canonical: the root is canonicalized here,
    // and children are read from an already-canonical parent (so they are
    // canonical too) except symlinks, which are resolved at push time. That
    // keeps the cycle guard exact without a `realpath(3)` per directory —
    // which matters at the scale this command advertises (a home directory).
    let mut stack: Vec<(PathBuf, usize)> = vec![(canonical(root), 0)];

    while let Some((canon, depth)) = stack.pop() {
        // Cycle/duplicate guard: a directory reachable twice (via symlinks, or
        // a root passed under two spellings) is walked once.
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
        on_dir(&canon);

        if is_dir(&canon.join(".engramdb")).await {
            let project_id = project_id::compute_project_id(&canon);
            let status = classify(&canon, &project_id, reg, &by_path, &by_id);
            report.projects.push(DiscoveredProject {
                path: canon.clone(),
                memory_count: count_memories(&canon, &project_id).await,
                project_id,
                status,
            });
            // Fall through: a project can contain nested projects (monorepo
            // packages), so keep descending — `.engramdb` itself is in
            // `DEFAULT_SKIP_DIRS`.
        }

        let Ok(mut entries) = async_fs::read_dir(&canon).await else {
            // Unreadable directory (permissions, dead mount): skip the subtree
            // but record it, so the caller can qualify an empty result.
            report.unreadable_dirs += 1;
            continue;
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                // A per-entry error must not silently truncate the rest of the
                // listing (`while let Ok(Some(_))` would drop every remaining
                // sibling); skip this one and keep reading.
                Err(_) => {
                    report.unreadable_dirs += 1;
                    continue;
                }
            };
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_symlink() && !opts.follow_symlinks {
                continue;
            }
            // `file_type` already answers dir-ness for everything but a
            // symlink, which needs a following `stat` to see its target.
            let is_directory = if file_type.is_symlink() {
                is_dir(&entry.path()).await
            } else {
                file_type.is_dir()
            };
            if !is_directory {
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
            let child = if file_type.is_symlink() {
                canonical(&entry.path())
            } else {
                entry.path()
            };
            stack.push((child, depth + 1));
        }
    }

    report.projects.sort_by(|a, b| a.path.cmp(&b.path));
    demote_intra_scan_id_collisions(&mut report.projects);
    Ok(report)
}

/// Re-classify unregistered candidates that collide with each other.
///
/// `classify` only sees the pre-scan registry snapshot, so two clones of one
/// git remote that are *both* missing from the registry — precisely the "I lost
/// `registry.json`" case this module exists for — each look `Unregistered`.
/// Adopting both would register only the first (`RegistryBackend::update` keeps
/// the first registration) while reporting success for both, and the second's
/// memories would be reindexed into the first's shared index.
///
/// Runs after the sort, so "first" is the lowest path and the outcome is
/// deterministic rather than dependent on directory-iteration order.
fn demote_intra_scan_id_collisions(projects: &mut [DiscoveredProject]) {
    let mut claimed: HashMap<String, PathBuf> = HashMap::new();
    for project in projects.iter_mut() {
        if project.status != DiscoveryStatus::Unregistered {
            continue;
        }
        match claimed.get(&project.project_id) {
            Some(owner) => {
                project.status = DiscoveryStatus::SharedId {
                    owner: owner.clone(),
                }
            }
            None => {
                claimed.insert(project.project_id.clone(), project.path.clone());
            }
        }
    }
}

/// Classify one project root against the registry indices.
fn classify(
    canon: &Path,
    project_id: &str,
    reg: &Registry,
    by_path: &HashSet<PathBuf>,
    by_id: &HashMap<String, &RegistryEntry>,
) -> DiscoveryStatus {
    // Worktree first: "never an independent project" is the strongest rule
    // here, and it must beat `StaleRegistration` — telling a worktree to run
    // `projects repair` would re-key its row to the worktree's own path hash
    // and detach it from main. A linked worktree's `.git` is a file, so
    // `compute_project_id` finds no `.git/config` and falls back to the path
    // hash, which is why it looks unregistered in the first place.
    //
    // Except when a row already points here: that is a worktree `resolve_
    // project_root` has linked as a sub-project — the healthy steady state, and
    // one that cannot be stale (its ID *is* the path hash, so it only moves if
    // the path does, and then the row would not point here). Reporting it as a
    // skipped worktree would flag every linked worktree on every scan.
    let worktree_main = project_id::detect_worktree_main(canon);
    if let Some(main) = worktree_main {
        if by_path.contains(canon) {
            return DiscoveryStatus::Registered;
        }
        return DiscoveryStatus::Worktree {
            main: canonical(&main),
        };
    }
    // Then drift, before `Registered`, via the same shared predicate the repair
    // and doctor paths use so the three can't disagree. Catches both real
    // shapes: one entry holding the stale ID, and the two-entry state that
    // running `init` on a re-keyed project produces.
    if let Some(stale) = crate::storage::stale_registrations_for(reg, canon, project_id).first() {
        return DiscoveryStatus::StaleRegistration {
            registered_id: stale.project_id.clone(),
        };
    }
    // A registry entry pointing here means it is correctly tracked.
    if by_path.contains(canon) {
        return DiscoveryStatus::Registered;
    }
    // The path isn't tracked, but the ID might be — two clones of the same git
    // remote share an ID. Only an owner that still exists is a conflict; a
    // vanished one is the moved-project case, which re-registration heals.
    if let Some(entry) = by_id.get(project_id) {
        let owner = canonical(Path::new(&entry.project_path));
        // Fail-closed for the same reason as the index above: an owner we
        // cannot stat is treated as present, so we skip rather than adopt.
        if owner != canon && owner.try_exists().unwrap_or(true) {
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

/// Memory files this project would index: the shared ones that travel with the
/// repo plus the machine-local personal ones.
///
/// Both are what `MemoryStore::reindex` walks, so counting only the shared dir
/// would tell a user "0 memories" for a project that indexes several — and
/// personal memories are exactly the ones that survive a lost `registry.json`,
/// the case this module exists for.
async fn count_memories(project_dir: &Path, project_id: &str) -> usize {
    let shared = count_memory_files(&paths::memories_dir(project_dir)).await;
    let personal = match paths::personal_memories_dir(project_id) {
        Ok(dir) => count_memory_files(&dir).await,
        Err(_) => 0,
    };
    shared + personal
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
        let owner_canon = owner.canonicalize().unwrap();
        let found: Vec<_> = report.skipped().collect();
        assert_eq!(found.len(), 1);
        assert!(matches!(
            &found[0].status,
            DiscoveryStatus::SharedId { owner: o } if o == &owner_canon
        ));
        assert_eq!(report.unregistered().count(), 0);
    }

    /// The registry-loss case: NEITHER clone is registered, so the pre-scan
    /// snapshot can't tell them apart. Without the post-pass both look
    /// adoptable, the second registration silently no-ops (the registry keeps
    /// the first), and its memories land in the first's index.
    #[tokio::test]
    async fn two_unregistered_clones_of_one_id_collapse_to_one_candidate() {
        let tmp = TempDir::new().unwrap();
        // Same fake git remote in both → identical project IDs.
        for name in ["a-clone", "z-clone"] {
            let dir = fake_project(tmp.path(), name, 0);
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            std::fs::write(
                dir.join(".git").join("config"),
                "[remote \"origin\"]\n\turl = git@github.com:acme/thing.git\n",
            )
            .unwrap();
        }

        let report = scan(tmp.path(), &Registry::default()).await;
        let candidates: Vec<_> = report.unregistered().collect();
        assert_eq!(
            candidates.len(),
            1,
            "only one of two same-ID clones may be offered"
        );
        assert!(candidates[0].path.ends_with("a-clone"), "lowest path wins");

        let skipped: Vec<_> = report.skipped().collect();
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].path.ends_with("z-clone"));
        assert!(matches!(
            &skipped[0].status,
            DiscoveryStatus::SharedId { owner } if owner.ends_with("a-clone")
        ));
    }

    /// A linked worktree's `.git` is a FILE, so `compute_project_id` finds no
    /// `.git/config` and falls back to the path hash — giving it an ID distinct
    /// from main's. It therefore looks unregistered, and adopting it would make
    /// a second root project owning main's memory files.
    #[tokio::test]
    async fn linked_worktree_is_skipped_not_offered_as_a_root_project() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        let wt = tmp.path().join("feature");
        let wt_gitdir = main.join(".git").join("worktrees").join("feature");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
        // The worktree carries a committed `.engramdb/` — the case that makes
        // it visible to discovery at all.
        fake_project(tmp.path(), "feature", 0);
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        let report = scan(tmp.path(), &Registry::default()).await;
        assert_eq!(
            report.unregistered().count(),
            0,
            "a worktree must never be offered as an independent project"
        );
        let skipped: Vec<_> = report.skipped().collect();
        assert_eq!(skipped.len(), 1);
        assert!(matches!(
            &skipped[0].status,
            DiscoveryStatus::Worktree { main: m } if m == &main.canonicalize().unwrap()
        ));

        // Once `resolve_project_root` has linked it as a sub-project there is a
        // row at this exact path, and the healthy steady state must read as
        // `Registered` — otherwise every linked worktree on the machine shows
        // up under "skipped" on every scan, which is noise, not a finding.
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: project_id::compute_project_id(&wt),
            project_path: wt.canonicalize().unwrap().to_string_lossy().to_string(),
            parent_project_id: Some(project_id::compute_project_id(&main)),
            subscriptions: vec![],
        });
        let report = scan(tmp.path(), &reg).await;
        assert_eq!(report.skipped().count(), 0);
        assert_eq!(report.unregistered().count(), 0);
        assert!(report
            .projects
            .iter()
            .any(|p| p.path == wt.canonicalize().unwrap()
                && matches!(p.status, DiscoveryStatus::Registered)));
    }

    /// State A: one entry, recorded under an ID the path no longer hashes to.
    /// It must not read as `Registered` (the tool for finding registry
    /// problems would declare the broken registration fine) nor as
    /// `Unregistered` (adopting adds a second row for one path).
    #[tokio::test]
    async fn a_rekeyed_registration_is_reported_not_adopted() {
        let tmp = TempDir::new().unwrap();
        let dir = fake_project(tmp.path(), "proj", 1);
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "stale00000000000".to_string(),
            project_path: dir.to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });

        let report = scan(tmp.path(), &reg).await;
        assert_eq!(report.unregistered().count(), 0);
        assert_eq!(report.registered().count(), 0);
        let skipped: Vec<_> = report.skipped().collect();
        assert_eq!(skipped.len(), 1);
        assert!(matches!(
            &skipped[0].status,
            DiscoveryStatus::StaleRegistration { registered_id } if registered_id == "stale00000000000"
        ));
    }

    /// State B: the two-row state that running `init` on an already-drifted
    /// project produces. The live ID IS registered, so a naive `by_path`
    /// lookup would call this healthy and leave the duplicate in place.
    #[tokio::test]
    async fn a_duplicate_registration_is_reported_too() {
        let tmp = TempDir::new().unwrap();
        let dir = fake_project(tmp.path(), "proj", 1);
        let live_id = project_id::compute_project_id(&dir);
        let mut reg = Registry::default();
        for id in ["stale00000000000", live_id.as_str()] {
            reg.projects.push(RegistryEntry {
                project_id: id.to_string(),
                project_path: dir.to_string_lossy().to_string(),
                parent_project_id: None,
                subscriptions: vec![],
            });
        }

        let report = scan(tmp.path(), &reg).await;
        assert_eq!(report.registered().count(), 0, "not healthy: two rows");
        assert_eq!(report.skipped().count(), 1);
    }

    #[tokio::test]
    async fn memory_count_includes_personal_memories() {
        // Personal memories live in the global data dir, not the project tree —
        // and are exactly what survives a lost registry, so a count that omits
        // them tells the user "0 memories" for a project that indexes several.
        let tmp = TempDir::new().unwrap();
        let dir = fake_project(tmp.path(), "proj", 2);
        let pid = project_id::compute_project_id(&dir);
        let personal = paths::personal_memories_dir(&pid).unwrap();
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::write(personal.join("p.md"), "---\n---\n").unwrap();

        let report = scan(tmp.path(), &Registry::default()).await;
        let found: Vec<_> = report.unregistered().collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].memory_count, 3, "2 shared + 1 personal");
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
