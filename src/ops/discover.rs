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
use futures_util::StreamExt;
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
    // `try_exists`, and an error counts as present: `exists()` collapses EACCES,
    // ESTALE and a hung network mount into "gone", and the failure mode here is
    // adopting (and reindexing) a live project's store.
    let alive = |p: &Path| p.try_exists().unwrap_or(true);
    for entry in &reg.projects {
        let entry_path = canonical(Path::new(&entry.project_path));
        by_path.insert(entry_path.clone());
        let live = alive(&entry_path);
        // A LIVING owner always wins the slot. `or_insert` gave it to whichever
        // row came first in the file, so a not-yet-pruned row for a deleted
        // checkout could occupy the ID of a project that is right there on disk
        // using it — `classify` then found the owner missing, called the clone
        // `Unregistered`, and adoption reindexed the live project's store to
        // empty. Reproduced; it flipped on registry row order alone.
        let mut claim = |id: String| match by_id.entry(id) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                let incumbent = canonical(Path::new(&slot.get().project_path));
                if live && !alive(&incumbent) {
                    slot.insert(entry);
                }
            }
        };
        claim(entry.project_id.clone());
        // Also index the ID the registered path hashes to TODAY. A drifted
        // project owns its live ID even though no row records it, and without
        // this a clone of that project matches nothing — so it is offered for
        // adoption, and its reindex clears the memories table the drifted
        // project is still using.
        if live {
            claim(project_id::compute_project_id(&entry_path));
        }
    }

    // The global/group stores live under the global data dir in the same
    // `.engramdb/` layout. They are engramdb's own storage, not user projects,
    // and are never registry entries — without this guard, scanning a home
    // directory would offer to "register" them.
    let internal_root = paths::global_data_dir().ok().map(|p| canonical(&p));

    let mut report = DiscoveryReport::default();

    // Breadth-first with bounded concurrency, one wave per depth level.
    //
    // Every directory costs a `read_dir` plus a `stat` per entry, and on the
    // scale this command advertises — a home directory, tens of thousands of
    // directories — that is pure I/O latency: a serial walk spends nearly all
    // of its wall clock waiting on one syscall at a time, and the work does
    // not shrink with a better algorithm. Sibling directories are entirely
    // independent, so running a bounded number of them at once turns N round
    // trips into roughly N/DIR_CONCURRENCY. The cap is what keeps a wide
    // directory from putting thousands of concurrent reads onto tokio's
    // blocking pool at once.
    //
    // Completion order is not part of the contract: the report is sorted by
    // path below, and `demote_intra_scan_id_collisions` runs after that sort,
    // so which sibling finishes first cannot change the outcome. `on_dir` is
    // progress reporting, so its order is free too.
    const DIR_CONCURRENCY: usize = 16;

    // Paths in the frontier are always canonical: the root is canonicalized
    // here, and children are read from an already-canonical parent (so they
    // are canonical too) except symlinks, which are resolved at push time.
    // That keeps the cycle guard exact without a `realpath(3)` per directory.
    let root_canon = canonical(root);
    let is_internal = |p: &Path| {
        internal_root
            .as_ref()
            .is_some_and(|internal| p.starts_with(internal))
    };
    // Cycle/duplicate guard: a directory reachable twice (via symlinks, or a
    // root passed under two spellings) is walked once. Membership is decided
    // when a child is queued rather than when it is popped; the serial walk
    // guarded at pop time and so also scanned it once — queueing it once just
    // saves the duplicate entry and the wasted pop. What DID change is which
    // depth a multiply-reachable directory gets: waves are depth-uniform, so
    // it is now always the shortest path from the root, where the old LIFO
    // pop order made it whichever spelling happened to win. That is only
    // observable under `follow_symlinks`, and it replaces a nondeterministic
    // answer with a deterministic one.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    visited.insert(root_canon.clone());
    let mut frontier: Vec<(PathBuf, usize)> = if is_internal(&root_canon) {
        Vec::new()
    } else {
        vec![(root_canon, 0)]
    };

    while !frontier.is_empty() {
        let wave = std::mem::take(&mut frontier);
        let scanned: Vec<DirScan> = futures_util::stream::iter(
            wave.into_iter()
                .map(|(canon, depth)| scan_one_dir(canon, depth, opts, &on_dir)),
        )
        .buffer_unordered(DIR_CONCURRENCY)
        .collect()
        .await;

        for scan in scanned {
            report.scanned_dirs += 1;
            report.unreadable_dirs += scan.unreadable_dirs;
            report.depth_limited |= scan.depth_limited;

            if let Some((project_id, memory_count)) = scan.project {
                // `classify` is pure and needs the borrowed registry indices,
                // so it stays on this side of the fan-out.
                let status = classify(&scan.canon, &project_id, reg, &by_path, &by_id);
                report.projects.push(DiscoveredProject {
                    path: scan.canon.clone(),
                    memory_count,
                    project_id,
                    status,
                });
                // A project can contain nested projects (monorepo packages),
                // so its children are still queued below — `.engramdb` itself
                // is in `DEFAULT_SKIP_DIRS`.
            }

            for child in scan.children {
                // The global/group stores live under the global data dir in
                // the same `.engramdb/` layout. They are engramdb's own
                // storage, not user projects, and are never registry entries —
                // without this guard, scanning a home directory would offer to
                // "register" them.
                if is_internal(&child) {
                    continue;
                }
                if visited.insert(child.clone()) {
                    frontier.push((child, scan.depth + 1));
                }
            }
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
/// Runs after the sort, so the outcome is deterministic rather than dependent
/// on directory-iteration order. Among colliding candidates the largest store
/// wins (see the tie-break note in the body); path order only settles a tie.
///
/// Two passes, and the split matters: **every** project in the scan claims its
/// ID, but only `Unregistered` ones can be demoted. Claiming from unregistered
/// candidates alone let a project we had just reported as drifted (or as
/// registered) fail to defend the ID it is visibly using, so a clone sharing
/// that ID stayed adoptable — and adoption reindexes, which empties the
/// original's memories table.
fn demote_intra_scan_id_collisions(projects: &mut [DiscoveredProject]) {
    // Pass 1: every project that is NOT a candidate defends its ID.
    let mut claimed: HashMap<String, PathBuf> = HashMap::new();
    for project in projects.iter() {
        if project.status != DiscoveryStatus::Unregistered {
            claimed.insert(project.project_id.clone(), project.path.clone());
        }
    }

    // Pass 2: among colliding candidates, the one with the MOST memories wins
    // the ID; path order only breaks an exact tie.
    //
    // Alphabetical order was not a safety property. Adoption reindexes, and a
    // reindex rebuilds the shared memories table from the adopted checkout's
    // files alone — so when a throwaway clone sorted before the real one, the
    // scratch copy took the ID and the real checkout's memories vanished from
    // every query until someone re-ran `reindex` there by hand. Whichever
    // clone is adopted, the other's memories are absent from the shared
    // index; picking the largest is the choice that loses least, and
    // `memory_count` is already computed for the prompt.
    //
    // Deterministic: `projects` is sorted by path before this runs, and the
    // comparison is (memory_count desc, path asc).
    let mut winners: HashMap<String, (usize, PathBuf)> = HashMap::new();
    for project in projects.iter() {
        if project.status != DiscoveryStatus::Unregistered
            || claimed.contains_key(&project.project_id)
        {
            continue;
        }
        match winners.get(&project.project_id) {
            Some((best, _)) if *best >= project.memory_count => {}
            _ => {
                winners.insert(
                    project.project_id.clone(),
                    (project.memory_count, project.path.clone()),
                );
            }
        }
    }

    for project in projects.iter_mut() {
        if project.status != DiscoveryStatus::Unregistered {
            continue;
        }
        // A non-candidate owner always beats a candidate.
        if let Some(owner) = claimed.get(&project.project_id) {
            project.status = DiscoveryStatus::SharedId {
                owner: owner.clone(),
            };
            continue;
        }
        match winners.get(&project.project_id) {
            Some((_, winner)) if winner != &project.path => {
                project.status = DiscoveryStatus::SharedId {
                    owner: winner.clone(),
                };
            }
            _ => {}
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
    let personal_dir = paths::personal_memories_dir(project_id).ok();
    // The shared and personal directories are on different filesystems as
    // often as not (one repo-adjacent, one under the global data dir), so
    // there is no reason to wait for the first read before starting the
    // second.
    let (shared, personal) = futures_util::future::join(
        count_memory_files(&paths::memories_dir(project_dir)),
        async {
            match &personal_dir {
                Some(dir) => count_memory_files(dir).await,
                None => 0,
            }
        },
    )
    .await;
    shared + personal
}

/// One directory's worth of scan results, produced off the critical path.
///
/// Kept deliberately data-only: the fan-out below borrows the registry
/// indices, and classification needs them, so this carries the *facts* a
/// directory read yields and leaves every registry decision to the driver.
struct DirScan {
    /// The canonical directory that was read.
    canon: PathBuf,
    /// Its depth below the scan root.
    depth: usize,
    /// `(project_id, memory_count)` when this directory holds `.engramdb/`.
    project: Option<(String, usize)>,
    /// Canonical child directories worth descending into.
    children: Vec<PathBuf>,
    /// Directories that could not be listed (this one, or entries within it
    /// whose reads failed).
    unreadable_dirs: usize,
    /// Whether a child was cut off by `max_depth`.
    depth_limited: bool,
}

/// Read one directory: detect a project, count its memories, and collect the
/// children to descend into.
///
/// Everything here is independent per directory, which is what makes the walk
/// safe to run several-at-a-time.
///
/// `on_dir` fires here, before the reads, rather than in the driver after the
/// wave lands. The driver sees a whole level at once, so reporting from there
/// made the spinner freeze for the duration of the widest level — the bulk of
/// the wall clock — and then repaint tens of thousands of already-finished
/// paths in a few milliseconds. "Scanning X" has to be said while X is being
/// scanned.
async fn scan_one_dir(
    canon: PathBuf,
    depth: usize,
    opts: &DiscoverOptions,
    on_dir: &(impl Fn(&Path) + Send + Sync),
) -> DirScan {
    on_dir(&canon);
    let mut out = DirScan {
        canon,
        depth,
        project: None,
        children: Vec::new(),
        unreadable_dirs: 0,
        depth_limited: false,
    };

    if is_dir(&out.canon.join(".engramdb")).await {
        let project_id = project_id::compute_project_id(&out.canon);
        let memory_count = count_memories(&out.canon, &project_id).await;
        out.project = Some((project_id, memory_count));
    }

    let Ok(mut entries) = async_fs::read_dir(&out.canon).await else {
        // Unreadable directory (permissions, dead mount): skip the subtree
        // but record it, so the caller can qualify an empty result.
        out.unreadable_dirs += 1;
        return out;
    };
    // Consecutive `next_entry` failures, reset by any success. A single bad
    // entry must not truncate the listing — but `next_entry` is not
    // guaranteed to advance past a failed `getdents` (a dead NFS mount
    // fails every call), and retrying forever would hang the scan with no
    // output at all. Bounded retry keeps the siblings and ends the spin.
    const MAX_CONSECUTIVE_ENTRY_ERRORS: u32 = 16;
    let mut entry_errors = 0u32;
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => {
                entry_errors = 0;
                entry
            }
            Ok(None) => break,
            // A per-entry error must not silently truncate the rest of the
            // listing (`while let Ok(Some(_))` would drop every remaining
            // sibling); skip this one and keep reading.
            Err(_) => {
                out.unreadable_dirs += 1;
                entry_errors += 1;
                if entry_errors >= MAX_CONSECUTIVE_ENTRY_ERRORS {
                    break;
                }
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
            out.depth_limited = true;
            continue;
        }
        let child = if file_type.is_symlink() {
            canonical(&entry.path())
        } else {
            entry.path()
        };
        out.children.push(child);
    }

    out
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

    async fn scan_with(root: &Path, reg: &Registry, opts: &DiscoverOptions) -> DiscoveryReport {
        discover_projects_in(root, reg, opts, |_| {}).await.unwrap()
    }

    /// A subtree the scan could not list must be COUNTED, because
    /// `unreadable_dirs` is the only thing that stops "no unregistered
    /// projects found" from being a claim of absence the scan cannot support.
    ///
    /// The count survived a serial->concurrent rewrite that moved it from one
    /// mutable counter to a per-directory field summed in the driver, and
    /// nothing pinned it either side of that move.
    ///
    /// Root ignores mode bits, so on a root runner there is no unreadable
    /// directory to observe and the assertion is skipped — CI runs
    /// `ubuntu-latest` as a normal user, where it does assert.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unlistable_directory_is_counted_and_does_not_truncate_the_scan() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let visible = fake_project(tmp.path(), "visible", 1);
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(locked.join("child")).unwrap();

        let set_mode = |mode: u32| {
            let mut perms = std::fs::metadata(&locked).unwrap().permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(&locked, perms).unwrap();
        };
        set_mode(0o000);
        let denied = std::fs::read_dir(&locked).is_err();
        let report = scan(tmp.path(), &Registry::default()).await;
        set_mode(0o755);

        if denied {
            assert_eq!(
                report.unreadable_dirs, 1,
                "an unlistable directory must be reported: {report:?}"
            );
            // And the failure must not have cost us the rest of the tree.
            assert!(
                report.projects.iter().any(|p| p.path.ends_with("visible")),
                "an unreadable sibling truncated the scan: {report:?}"
            );
        }
        drop(visible);
    }

    /// One directory reachable by two names is scanned once, and a symlink
    /// cycle terminates.
    ///
    /// `follow_symlinks` had no test at all, which is also what makes the
    /// canonicalize-at-push-time invariant (the thing keeping the cycle guard
    /// exact) unpinned.
    #[cfg(unix)]
    #[tokio::test]
    async fn following_symlinks_visits_a_shared_target_once_and_survives_a_cycle() {
        let tmp = TempDir::new().unwrap();
        let shared = fake_project(tmp.path(), "shared", 3);
        std::fs::create_dir_all(tmp.path().join("p1")).unwrap();
        std::fs::create_dir_all(tmp.path().join("p2")).unwrap();
        // Two routes to one project...
        std::os::unix::fs::symlink(&shared, tmp.path().join("p1").join("link")).unwrap();
        std::os::unix::fs::symlink(&shared, tmp.path().join("p2").join("link")).unwrap();
        // ...and a self-cycle, which a walk without a canonical visited set
        // would follow until it hit max_depth (or forever).
        std::os::unix::fs::symlink(tmp.path().join("p1"), tmp.path().join("p1").join("loop"))
            .unwrap();

        let opts = DiscoverOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let report = scan_with(tmp.path(), &Registry::default(), &opts).await;

        let found: Vec<_> = report
            .projects
            .iter()
            .filter(|p| p.path.ends_with("shared"))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "a project reachable by two symlinks was reported twice: {report:?}"
        );
        // Reported once means it is still adoptable — being demoted to
        // `SharedId` against itself would be the subtler failure.
        assert_eq!(found[0].status, DiscoveryStatus::Unregistered);
    }

    /// The internal-store guard must hold for a CHILD, not just for the root.
    ///
    /// Scanning the global data dir *itself* hits the root short-circuit and
    /// empties the frontier before the per-child guard can run — so that test
    /// alone would still pass with the child guard deleted. The real case is
    /// scanning a home directory, where the global store is reached as a
    /// descendant and would otherwise be offered for adoption: engramdb
    /// proposing to register its own global store as a user project.
    #[tokio::test]
    async fn the_internal_store_is_skipped_when_reached_as_a_child() {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ENGRAMDB_DATA_DIR", tmp.path().join("data"));
        let internal = paths::global_data_dir().unwrap();
        std::fs::create_dir_all(internal.join("projects").join("global").join(".engramdb"))
            .unwrap();
        // A real project alongside it, so an empty result cannot pass by
        // accident.
        fake_project(tmp.path(), "real", 1);

        let opts = DiscoverOptions {
            include_hidden: true,
            ..Default::default()
        };
        let report = scan_with(tmp.path(), &Registry::default(), &opts).await;

        assert!(
            report
                .projects
                .iter()
                .all(|p| !p.path.starts_with(&internal)),
            "engramdb's own global store was offered as a user project: {report:?}"
        );
        assert_eq!(report.projects.len(), 1);
    }

    /// Among colliding candidates the LARGEST store wins the ID, whichever way
    /// they sort.
    ///
    /// The tie-break used to be alphabetical, which is not a safety property:
    /// adoption reindexes, and a reindex rebuilds the shared memories table
    /// from the adopted checkout alone — so a throwaway clone sorting first
    /// took the ID and the real checkout's memories vanished from every query.
    /// The scratch copy sorts FIRST here, which is the losing order for the
    /// old rule.
    #[tokio::test]
    async fn the_largest_colliding_candidate_wins_the_id() {
        let mut projects = vec![
            DiscoveredProject {
                path: PathBuf::from("/a-scratch"),
                project_id: "shared0000000000".to_string(),
                memory_count: 0,
                status: DiscoveryStatus::Unregistered,
            },
            DiscoveredProject {
                path: PathBuf::from("/z-real"),
                project_id: "shared0000000000".to_string(),
                memory_count: 400,
                status: DiscoveryStatus::Unregistered,
            },
        ];
        demote_intra_scan_id_collisions(&mut projects);

        assert_eq!(
            projects[1].status,
            DiscoveryStatus::Unregistered,
            "the checkout with 400 memories must keep the id"
        );
        assert_eq!(
            projects[0].status,
            DiscoveryStatus::SharedId {
                owner: PathBuf::from("/z-real")
            },
            "the empty scratch clone must be demoted"
        );
    }

    /// A project that is not a candidate defends its ID even when it sorts
    /// LAST — the two-pass split is what makes that true, and a single-pass
    /// "claim as you go" implementation passes every other test in this file.
    #[tokio::test]
    async fn a_defender_that_sorts_last_still_holds_its_id() {
        let mut projects = vec![
            DiscoveredProject {
                path: PathBuf::from("/a-clone"),
                project_id: "shared0000000000".to_string(),
                memory_count: 99,
                status: DiscoveryStatus::Unregistered,
            },
            DiscoveredProject {
                path: PathBuf::from("/z-drifted"),
                project_id: "shared0000000000".to_string(),
                memory_count: 1,
                status: DiscoveryStatus::StaleRegistration {
                    registered_id: "old00000000000".to_string(),
                },
            },
        ];
        demote_intra_scan_id_collisions(&mut projects);

        // Even with 99 memories against 1, a candidate never takes an ID a
        // non-candidate is visibly using.
        assert_eq!(
            projects[0].status,
            DiscoveryStatus::SharedId {
                owner: PathBuf::from("/z-drifted")
            },
            "a clone took an id the drifted project is still using"
        );
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

    /// A dead registry row must not be able to hold an ID that a project
    /// *visible in this very scan* is using.
    ///
    /// The `by_id` index gave the slot to whichever row came first in the file.
    /// With a not-yet-pruned row for a deleted checkout ahead of the live
    /// project's own (drifted) row, the clone matched a missing owner, came out
    /// `Unregistered`, and adoption reindexed the live project's store to empty.
    /// Reproduced end-to-end; it flipped on registry row order alone, so the
    /// test pins the losing order.
    #[tokio::test]
    async fn a_dead_row_cannot_shield_an_id_a_live_project_is_using() {
        let tmp = TempDir::new().unwrap();
        let live = fake_project(tmp.path(), "live", 2);
        let clone = fake_project(tmp.path(), "clone", 0);
        // Both hash to the same ID only if they share a remote; without git
        // they hash by path, so force the collision the way the registry sees
        // it: the live project is registered under a STALE id (drifted), and a
        // dead row holds the id the clone hashes to.
        let clone_id = project_id::compute_project_id(&clone);
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: clone_id.clone(),
            project_path: tmp.path().join("deleted-checkout").to_string_lossy().into(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        reg.projects.push(RegistryEntry {
            project_id: clone_id.clone(),
            project_path: live.to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });

        let report = scan(tmp.path(), &reg).await;
        let offered: Vec<_> = report.unregistered().map(|p| p.path.clone()).collect();
        assert!(
            !offered.contains(&clone.canonicalize().unwrap()),
            "adopting this reindexes the live project's store to empty: {offered:?}"
        );
    }

    /// The intra-scan pass must let EVERY project defend its ID, not just the
    /// unregistered ones — a project reported as drifted is visibly using its
    /// live ID, and a clone sharing it must not stay adoptable.
    #[tokio::test]
    async fn a_drifted_project_in_the_scan_defends_its_live_id() {
        let tmp = TempDir::new().unwrap();
        let drifted = fake_project(tmp.path(), "a-drifted", 2);
        let clone = fake_project(tmp.path(), "b-clone", 0);
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "stale00000000000".to_string(),
            project_path: drifted.to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });

        let mut report = scan(tmp.path(), &reg).await;
        // Force the collision: give the clone the drifted project's live ID,
        // then re-run the intra-scan pass over the result.
        let live_id = project_id::compute_project_id(&drifted);
        for p in report.projects.iter_mut() {
            if p.path == clone.canonicalize().unwrap() {
                p.project_id = live_id.clone();
            }
        }
        super::demote_intra_scan_id_collisions(&mut report.projects);

        let clone_status = report
            .projects
            .iter()
            .find(|p| p.path == clone.canonicalize().unwrap())
            .map(|p| p.status.clone())
            .unwrap();
        assert!(
            matches!(clone_status, DiscoveryStatus::SharedId { .. }),
            "the drifted project owns this ID; got {clone_status:?}"
        );
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
