//! Every store on the machine: registered projects (with their parent/child
//! links), the global store and the group stores.
//!
//! Built from the registry and each store's `manifest.toml` — never by
//! opening a store, so listing a machine with fifty projects costs fifty
//! small file reads, not fifty LanceDB connections. A project whose path is
//! gone, or that never had a store of its own (a worktree), is listed as
//! unavailable with the reason; it never fails the whole list.
//!
//! Every filesystem probe runs on the blocking pool (`spawn_blocking`, or
//! `tokio::fs`, which does the same), all rows at once, each under
//! [`ROW_TIMEOUT`]: a registered project on a hung network mount reads as
//! "timed out", and neither stalls a runtime worker nor the other rows. The
//! stuck probe thread itself is not cancellable and finishes (or not) on its
//! own — that is the declared cost. [`off_runtime`] keeps that cost bounded:
//! at most one probe per key (a path, a store) is ever running, and a key
//! whose probe timed out is answered "timed out" without a new thread for
//! [`PROBE_RETRY_AFTER`], so reloading a page over a hung mount cannot fill
//! the blocking pool (every `tokio::fs` call queues behind a full one).
//!
//! A project's store is keyed by the id computed from its path *now*
//! (`compute_project_id`, which prefers the git remote), exactly as
//! `MemoryStore::open_read_only` keys it — not by the id the registry
//! recorded, which differs once a remote is added after registration. The
//! registry id stays the row's API id (an alias `locate` resolves).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use engramdb::storage::{
    detect_worktree_main, manifest, paths, project_id::compute_project_id, FileRegistry,
    MemoryStore, Registry, RegistryBackend, SchemaState,
};
use serde::Serialize;

use crate::{ApiError, CurrentProject};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Project,
    Global,
    Group,
}

/// Where a store lives: what [`crate::stores::StoreCache`] opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub id: String,
    pub kind: StoreKind,
    pub root: PathBuf,
}

/// One row of `/api/projects`. Every string here except `id` and `kind` is
/// someone else's text (a path, a manifest name): data, escaped by the
/// frontend.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub kind: StoreKind,
    pub path: String,
    /// The project of the directory the dashboard started in.
    pub current: bool,
    /// It has a store this dashboard can read.
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub children: Vec<String>,
    /// A linked git worktree (routes to its main worktree's store).
    pub worktree: bool,
    /// Groups this project is subscribed to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<String>,
    /// Projects subscribed to this group.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subscribers: Vec<String>,
    /// From the manifest's stats (kept current by every write); `None` when
    /// unreadable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_count: Option<usize>,
    /// When the memories directory last changed (a file added, renamed or
    /// removed — every write is a rename).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_write_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub current: Option<String>,
    pub projects: Vec<ProjectView>,
}

async fn load_registry() -> Result<Registry, ApiError> {
    FileRegistry::global()
        .map_err(ApiError::internal)?
        .load()
        .await
        .map_err(|e| ApiError::internal(format!("cannot read the project registry: {e}")))
}

/// How long one store's probes may take before its row reads as timed out.
pub const ROW_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a key whose probe timed out is refused without a new probe.
pub const PROBE_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Keys kept in [`off_runtime`]'s table before idle ones are dropped.
const GATE_TABLE_MAX: usize = 512;

/// Why [`off_runtime`] has no result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// It ran (or waited for the key's running probe) past its limit.
    TimedOut(Duration),
    /// The key's last probe timed out less than [`PROBE_RETRY_AFTER`] ago
    /// (or one is still stuck): not retried yet.
    RecentlyTimedOut,
    /// The work panicked.
    Failed,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::TimedOut(d) => write!(f, "timed out after {}s", d.as_secs()),
            Refused::RecentlyTimedOut => write!(
                f,
                "timed out less than {}s ago (not retried yet)",
                PROBE_RETRY_AFTER.as_secs()
            ),
            Refused::Failed => f.write_str("the probe failed"),
        }
    }
}

struct Gate {
    one: std::sync::Arc<tokio::sync::Semaphore>,
    timed_out: Option<std::time::Instant>,
}

fn gates() -> &'static std::sync::Mutex<HashMap<String, Gate>> {
    static GATES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Gate>>> =
        std::sync::OnceLock::new();
    GATES.get_or_init(Default::default)
}

/// Run `work` off the runtime workers — on the blocking pool, under its own
/// `block_on`, so a synchronous syscall inside it (`is_dir`, `canonicalize`,
/// `read_to_string` of a `.git` file) on a hung mount pins a blocking thread
/// and never a worker — within `limit`, and **at most one at a time per
/// `key`**: a second caller waits for the running one (within its own
/// `limit`), and once one times out the key is refused for
/// [`PROBE_RETRY_AFTER`] without starting another. So a hung mount costs one
/// stuck thread per key, however often a page reloads. A timed-out work is
/// left to finish on its own (it cannot be cancelled) and holds its key
/// until it does.
pub(crate) async fn off_runtime<T, F>(key: String, limit: Duration, work: F) -> Result<T, Refused>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let one = {
        let mut table = gates().lock().unwrap_or_else(|p| p.into_inner());
        if table.len() >= GATE_TABLE_MAX {
            table.retain(|_, g| {
                g.one.available_permits() == 0
                    || g.timed_out.is_some_and(|t| t.elapsed() < PROBE_RETRY_AFTER)
            });
        }
        let gate = table.entry(key.clone()).or_insert_with(|| Gate {
            one: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            timed_out: None,
        });
        if let Some(t) = gate.timed_out {
            if t.elapsed() < PROBE_RETRY_AFTER || gate.one.available_permits() == 0 {
                return Err(Refused::RecentlyTimedOut);
            }
            gate.timed_out = None;
        }
        gate.one.clone()
    };
    let rt = tokio::runtime::Handle::current();
    let run = async move {
        // Never closed, so `acquire_owned` only waits.
        let permit = one.acquire_owned().await.ok()?;
        tokio::task::spawn_blocking(move || {
            let _held_until_done = permit;
            rt.block_on(work)
        })
        .await
        .ok()
    };
    match tokio::time::timeout(limit, run).await {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(Refused::Failed),
        Err(_) => {
            let mut table = gates().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(g) = table.get_mut(&key) {
                g.timed_out = Some(std::time::Instant::now());
            }
            Err(Refused::TimedOut(limit))
        }
    }
}

/// `try_exists` with an error counted as present-but-unreadable, so an
/// unreadable mount reads as "cannot read", not "gone".
fn probe(path: &Path) -> Result<bool, String> {
    path.try_exists().map_err(|e| e.to_string())
}

fn dir_mtime(path: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

/// Where a store's LanceDB index is.
#[derive(Clone)]
enum IndexDir {
    /// A fixed directory (groups, the global store).
    At(Option<PathBuf>),
    /// A project's: keyed by the id computed from its path.
    OfProject,
}

#[derive(Default)]
struct StoreFacts {
    /// The root directory exists.
    exists: bool,
    /// It has a `.engramdb/` of its own.
    has_store_dir: bool,
    /// It is a linked git worktree.
    worktree: bool,
    /// The id its store is keyed by (projects only).
    store_id: Option<String>,
    available: bool,
    reason: Option<String>,
    name: Option<String>,
    memory_count: Option<usize>,
    last_write_at: Option<DateTime<Utc>>,
    schema: Option<SchemaState>,
}

/// The synchronous half of [`facts`], for the blocking pool.
fn probe_blocking(root: &Path, index: &IndexDir) -> StoreFacts {
    let mut f = StoreFacts::default();
    match probe(root) {
        Ok(true) => f.exists = true,
        Ok(false) => {
            f.reason = Some("the project directory no longer exists".into());
            return f;
        }
        Err(e) => {
            f.reason = Some(format!("cannot read the project directory: {e}"));
            return f;
        }
    }
    f.worktree = detect_worktree_main(root).is_some();
    match probe(&paths::project_dir(root)) {
        Ok(true) => f.has_store_dir = true,
        Ok(false) => {
            f.reason = Some("no EngramDB store in this directory".into());
            return f;
        }
        Err(e) => {
            f.reason = Some(format!("cannot read .engramdb: {e}"));
            return f;
        }
    }
    f.last_write_at = dir_mtime(&paths::memories_dir(root));
    let lance = match index {
        IndexDir::At(dir) => dir.clone(),
        IndexDir::OfProject => {
            let id = compute_project_id(root);
            let dir = paths::lancedb_dir(&id).ok();
            f.store_id = Some(id);
            dir
        }
    };
    if lance.is_some_and(|d| !d.is_dir()) {
        f.reason = Some(
            "not indexed on this machine yet: run any engramdb command there to build it".into(),
        );
        return f;
    }
    f.available = true;
    f
}

/// What the directory and the manifest say about a store rooted at `root`,
/// read without opening it, off the runtime workers, within [`ROW_TIMEOUT`].
async fn facts(root: PathBuf, index: IndexDir) -> StoreFacts {
    let key = format!("dir:{}", root.display());
    let work = async move {
        // Already on a blocking thread (`off_runtime`).
        let mut f = probe_blocking(&root, &index);
        if f.has_store_dir {
            let engramdb_dir = paths::project_dir(&root);
            if let Ok(m) = manifest::load_manifest(&engramdb_dir.join("manifest.toml")).await {
                f.name = Some(m.project);
                f.memory_count = Some(m.stats.memory_count);
            }
            f.schema = Some(MemoryStore::schema_state_at(&root).await);
        }
        f
    };
    match off_runtime(key, ROW_TIMEOUT, work).await {
        Ok(f) => f,
        Err(Refused::Failed) => StoreFacts {
            reason: Some("probe failed".into()),
            ..StoreFacts::default()
        },
        Err(e) => StoreFacts {
            reason: Some(format!("{e} reading the directory (an unreachable mount?)")),
            ..StoreFacts::default()
        },
    }
}

/// [`facts`] for many stores at once.
async fn facts_all(jobs: Vec<(PathBuf, IndexDir)>) -> Vec<StoreFacts> {
    let mut set = tokio::task::JoinSet::new();
    for (i, (root, index)) in jobs.into_iter().enumerate() {
        set.spawn(async move { (i, facts(root, index).await) });
    }
    let mut out: Vec<Option<StoreFacts>> = (0..set.len()).map(|_| None).collect();
    while let Some(r) = set.join_next().await {
        if let Ok((i, f)) = r {
            out[i] = Some(f);
        }
    }
    out.into_iter()
        .map(|f| {
            f.unwrap_or_else(|| StoreFacts {
                reason: Some("probe failed".into()),
                ..StoreFacts::default()
            })
        })
        .collect()
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Which rows to build.
enum Want<'a> {
    All,
    One(&'a str),
}

impl Want<'_> {
    fn has(&self, id: &str) -> bool {
        match self {
            Want::All => true,
            Want::One(w) => *w == id,
        }
    }
}

fn empty_reason(kind: StoreKind, r: String) -> String {
    let empty = r.starts_with("the project directory") || r.starts_with("no EngramDB");
    match kind {
        StoreKind::Group if empty => "no memories written to this group yet".into(),
        StoreKind::Global if empty => "no global memories yet".into(),
        _ => r,
    }
}

/// Every store on the machine, current project first, each root followed by
/// its children, then groups, then the global store.
pub async fn catalog(current: Option<&CurrentProject>) -> Result<Catalog, ApiError> {
    let rows = build(current, Want::All).await?;
    Ok(Catalog {
        current: rows
            .iter()
            .find(|r| r.current)
            .map(|r| r.id.clone())
            .or_else(|| current.map(|c| c.id.clone())),
        projects: rows,
    })
}

/// One row of the catalog, probing only that store.
pub async fn view(
    id: &str,
    current: Option<&CurrentProject>,
) -> Result<Option<ProjectView>, ApiError> {
    Ok(build(current, Want::One(id)).await?.into_iter().next())
}

async fn build(
    current: Option<&CurrentProject>,
    want: Want<'_>,
) -> Result<Vec<ProjectView>, ApiError> {
    let registry = load_registry().await?;
    let mut children: HashMap<&str, Vec<String>> = HashMap::new();
    let mut subscribers: HashMap<&str, Vec<String>> = HashMap::new();
    for e in &registry.projects {
        if let Some(p) = &e.parent_project_id {
            children
                .entry(p.as_str())
                .or_default()
                .push(e.project_id.clone());
        }
        for g in &e.subscriptions {
            subscribers
                .entry(g.as_str())
                .or_default()
                .push(e.project_id.clone());
        }
    }

    // Projects. The registry holds one row per id, and two checkouts of one
    // remote share an id; the one the dashboard started in is the one shown.
    let entries: Vec<_> = registry
        .projects
        .iter()
        .filter(|e| want.has(&e.project_id))
        .collect();
    let roots: Vec<PathBuf> = entries
        .iter()
        .map(|e| match current {
            Some(c) if c.id == e.project_id => c.root.clone(),
            _ => PathBuf::from(&e.project_path),
        })
        .collect();
    let probed = facts_all(
        roots
            .iter()
            .map(|r| (r.clone(), IndexDir::OfProject))
            .collect(),
    )
    .await;

    let mut rows: BTreeMap<String, ProjectView> = BTreeMap::new();
    for ((e, root), f) in entries.iter().zip(roots).zip(probed) {
        // Current by either id: the recorded one, or the one computed from
        // the path now (they differ once a remote was added later).
        let is_current = current
            .is_some_and(|c| c.id == e.project_id || f.store_id.as_deref() == Some(c.id.as_str()));
        let mut reason = f.reason;
        if let Some(p) = e
            .parent_project_id
            .as_ref()
            .filter(|_| f.exists && !f.has_store_dir)
        {
            reason = Some(format!("sub-project: its memories live in {p}"));
        }
        rows.insert(
            e.project_id.clone(),
            ProjectView {
                id: e.project_id.clone(),
                name: f.name.unwrap_or_else(|| basename(&root)),
                kind: StoreKind::Project,
                path: root.display().to_string(),
                current: is_current,
                available: f.available,
                unavailable_reason: reason,
                parent_id: e.parent_project_id.clone(),
                children: children
                    .get(e.project_id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                worktree: f.worktree,
                subscriptions: e.subscriptions.clone(),
                subscribers: Vec::new(),
                memory_count: f.memory_count,
                last_write_at: f.last_write_at,
                schema: f.schema,
            },
        );
    }
    // A current project missing from the registry (registered under another
    // machine's data dir, or a registry that was reset) is still shown.
    if let Some(c) = current {
        if want.has(&c.id) && !rows.values().any(|r| r.current) {
            let f = facts(c.root.clone(), IndexDir::OfProject).await;
            rows.insert(
                c.id.clone(),
                ProjectView {
                    id: c.id.clone(),
                    name: f.name.unwrap_or_else(|| basename(&c.root)),
                    kind: StoreKind::Project,
                    path: c.root.display().to_string(),
                    current: true,
                    available: f.available,
                    unavailable_reason: f.reason,
                    parent_id: None,
                    children: Vec::new(),
                    worktree: false,
                    subscriptions: Vec::new(),
                    subscribers: Vec::new(),
                    memory_count: f.memory_count,
                    last_write_at: f.last_write_at,
                    schema: f.schema,
                },
            );
        }
    }

    // Order: current first, then roots by name with their descendants right
    // after them (depth-first), then anything left (cycles, dangling parents).
    let mut ordered: Vec<ProjectView> = Vec::new();
    let mut placed = std::collections::HashSet::new();
    fn place(
        id: &str,
        rows: &BTreeMap<String, ProjectView>,
        placed: &mut std::collections::HashSet<String>,
        out: &mut Vec<ProjectView>,
    ) {
        if !placed.insert(id.to_string()) {
            return;
        }
        let Some(row) = rows.get(id) else { return };
        out.push(row.clone());
        let mut kids: Vec<&ProjectView> = row.children.iter().filter_map(|c| rows.get(c)).collect();
        kids.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        for k in kids {
            place(&k.id, rows, placed, out);
        }
    }
    if let Some(cur) = rows.values().find(|r| r.current).map(|r| r.id.clone()) {
        place(&cur, &rows, &mut placed, &mut ordered);
    }
    let mut roots: Vec<&ProjectView> = rows
        .values()
        .filter(|r| r.parent_id.as_ref().is_none_or(|p| !rows.contains_key(p)))
        .collect();
    roots.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    let root_ids: Vec<String> = roots.iter().map(|r| r.id.clone()).collect();
    for id in root_ids {
        place(&id, &rows, &mut placed, &mut ordered);
    }
    let rest: Vec<String> = rows.keys().cloned().collect();
    for id in rest {
        place(&id, &rows, &mut placed, &mut ordered);
    }

    // Groups, then the global store.
    let mut others: Vec<(String, String, StoreKind, PathBuf, IndexDir)> = Vec::new();
    for g in registry.groups.iter().filter(|g| want.has(&g.group_id)) {
        others.push((
            g.group_id.clone(),
            g.name.clone(),
            StoreKind::Group,
            paths::group_store_dir(&g.group_id).map_err(ApiError::internal)?,
            IndexDir::At(paths::group_lancedb_dir(&g.group_id).ok()),
        ));
    }
    if want.has(paths::GLOBAL_PROJECT_ID) {
        others.push((
            paths::GLOBAL_PROJECT_ID.to_string(),
            "Global store".to_string(),
            StoreKind::Global,
            paths::global_store_dir().map_err(ApiError::internal)?,
            IndexDir::At(paths::global_lancedb_dir().ok()),
        ));
    }
    let probed = facts_all(
        others
            .iter()
            .map(|(_, _, _, root, index)| (root.clone(), index.clone()))
            .collect(),
    )
    .await;
    for ((id, name, kind, root, _), f) in others.into_iter().zip(probed) {
        ordered.push(ProjectView {
            subscribers: subscribers.get(id.as_str()).cloned().unwrap_or_default(),
            id,
            name,
            kind,
            path: root.display().to_string(),
            current: false,
            available: f.available,
            unavailable_reason: f.reason.map(|r| empty_reason(kind, r)),
            parent_id: None,
            children: Vec::new(),
            worktree: false,
            subscriptions: Vec::new(),
            memory_count: f.memory_count,
            last_write_at: f.last_write_at,
            schema: f.schema,
        });
    }
    Ok(ordered)
}

/// Resolve an API project id to its store. 404 for an id the machine does
/// not know; 409 (`unavailable`) for one that is known but has no readable
/// store right now.
pub async fn locate(id: &str, current: Option<&CurrentProject>) -> Result<Location, ApiError> {
    if id == paths::GLOBAL_PROJECT_ID {
        return Ok(Location {
            id: id.to_string(),
            kind: StoreKind::Global,
            root: paths::global_store_dir().map_err(ApiError::internal)?,
        });
    }
    if let Some(c) = current.filter(|c| c.id == id) {
        return Ok(Location {
            id: id.to_string(),
            kind: StoreKind::Project,
            root: c.root.clone(),
        });
    }
    let registry = load_registry().await?;
    if registry.groups.iter().any(|g| g.group_id == id) {
        return Ok(Location {
            id: id.to_string(),
            kind: StoreKind::Group,
            root: paths::group_store_dir(id).map_err(ApiError::internal)?,
        });
    }
    let Some(entry) = registry.projects.iter().find(|e| e.project_id == id) else {
        return Err(ApiError::not_found(
            "unknown_project",
            format!("no project {id} is registered on this machine"),
        ));
    };
    let root = PathBuf::from(&entry.project_path);
    let has_parent = entry.parent_project_id.clone();
    // Off the runtime workers, bounded like a catalog row (and sharing its
    // key, so a hung directory has one probe in flight at most).
    let probe_root = root.clone();
    let probed = off_runtime(format!("dir:{}", root.display()), ROW_TIMEOUT, async move {
        (
            paths::project_dir(&probe_root).is_dir(),
            probe_root.is_dir(),
        )
    })
    .await;
    let (has_store, root_is_dir) = match probed {
        Ok(v) => v,
        Err(Refused::Failed) => return Err(ApiError::internal("probe failed")),
        Err(e) => {
            return Err(ApiError::unavailable(format!(
                "{e} reading {} (an unreachable mount?)",
                root.display()
            )))
        }
    };
    if !has_store {
        let reason = match &has_parent {
            Some(p) if root_is_dir => format!("sub-project: its memories live in {p}"),
            _ if !root_is_dir => "the project directory no longer exists".into(),
            _ => "no EngramDB store in this directory".into(),
        };
        return Err(ApiError::unavailable(reason));
    }
    Ok(Location {
        id: id.to_string(),
        kind: StoreKind::Project,
        root,
    })
}
