//! Every store on the machine: registered projects (with their parent/child
//! links), the global store and the group stores.
//!
//! Built from the registry and each store's `manifest.toml` — never by
//! opening a store, so listing a machine with fifty projects costs fifty
//! small file reads, not fifty LanceDB connections. A project whose path is
//! gone, or that never had a store of its own (a worktree), is listed as
//! unavailable with the reason; it never fails the whole list.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use engramdb::storage::{
    detect_worktree_main, manifest, paths, FileRegistry, MemoryStore, Registry, RegistryBackend,
    SchemaState,
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

/// `try_exists` with an error counted as present-but-unreadable, so a hung
/// mount reads as "unavailable: cannot read", not "gone".
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

struct StoreFacts {
    available: bool,
    reason: Option<String>,
    name: Option<String>,
    memory_count: Option<usize>,
    last_write_at: Option<DateTime<Utc>>,
    schema: Option<SchemaState>,
}

/// What the manifest and the directory say about a store rooted at `root`,
/// read without opening it.
async fn facts(root: &Path, lancedb: Option<PathBuf>) -> StoreFacts {
    let engramdb_dir = paths::project_dir(root);
    let mut f = StoreFacts {
        available: false,
        reason: None,
        name: None,
        memory_count: None,
        last_write_at: None,
        schema: None,
    };
    match probe(root) {
        Ok(true) => {}
        Ok(false) => {
            f.reason = Some("the project directory no longer exists".into());
            return f;
        }
        Err(e) => {
            f.reason = Some(format!("cannot read the project directory: {e}"));
            return f;
        }
    }
    match probe(&engramdb_dir) {
        Ok(true) => {}
        Ok(false) => {
            f.reason = Some("no EngramDB store in this directory".into());
            return f;
        }
        Err(e) => {
            f.reason = Some(format!("cannot read .engramdb: {e}"));
            return f;
        }
    }
    if let Ok(m) = manifest::load_manifest(&engramdb_dir.join("manifest.toml")).await {
        f.name = Some(m.project);
        f.memory_count = Some(m.stats.memory_count);
    }
    f.last_write_at = dir_mtime(&paths::memories_dir(root));
    f.schema = Some(MemoryStore::schema_state_at(root).await);
    if let Some(lance) = lancedb {
        if !lance.is_dir() {
            f.reason = Some(
                "not indexed on this machine yet: run any engramdb command there to build it"
                    .into(),
            );
            return f;
        }
    }
    f.available = true;
    f
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Every store on the machine, current project first, each root followed by
/// its children, then groups, then the global store.
pub async fn catalog(current: Option<&CurrentProject>) -> Result<Catalog, ApiError> {
    let registry = load_registry().await?;
    let mut children: HashMap<&str, Vec<String>> = HashMap::new();
    for e in &registry.projects {
        if let Some(p) = &e.parent_project_id {
            children
                .entry(p.as_str())
                .or_default()
                .push(e.project_id.clone());
        }
    }
    let mut subscribers: HashMap<&str, Vec<String>> = HashMap::new();
    for e in &registry.projects {
        for g in &e.subscriptions {
            subscribers
                .entry(g.as_str())
                .or_default()
                .push(e.project_id.clone());
        }
    }

    let mut rows: BTreeMap<String, ProjectView> = BTreeMap::new();
    for e in &registry.projects {
        let is_current = current.is_some_and(|c| c.id == e.project_id);
        // The registry holds one row per id, and two checkouts of one remote
        // share an id; the one the dashboard started in is the one to show.
        let root = match current {
            Some(c) if is_current => c.root.clone(),
            _ => PathBuf::from(&e.project_path),
        };
        let lance = paths::lancedb_dir(&e.project_id).ok();
        let mut f = facts(&root, lance).await;
        let exists = probe(&root).unwrap_or(false);
        let worktree = exists && detect_worktree_main(&root).is_some();
        if let Some(p) = e
            .parent_project_id
            .as_ref()
            .filter(|_| exists && !f.available)
        {
            if !paths::project_dir(&root).is_dir() {
                f.reason = Some(format!("sub-project: its memories live in {p}"));
            }
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
                unavailable_reason: f.reason,
                parent_id: e.parent_project_id.clone(),
                children: children
                    .get(e.project_id.as_str())
                    .cloned()
                    .unwrap_or_default(),
                worktree,
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
        if !rows.contains_key(&c.id) {
            let f = facts(&c.root, paths::lancedb_dir(&c.id).ok()).await;
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
    if let Some(c) = current {
        place(&c.id, &rows, &mut placed, &mut ordered);
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

    for g in &registry.groups {
        let root = paths::group_store_dir(&g.group_id).map_err(ApiError::internal)?;
        let f = facts(&root, paths::group_lancedb_dir(&g.group_id).ok()).await;
        ordered.push(ProjectView {
            id: g.group_id.clone(),
            name: g.name.clone(),
            kind: StoreKind::Group,
            path: root.display().to_string(),
            current: false,
            available: f.available,
            unavailable_reason: f.reason.map(|r| {
                if r.starts_with("the project directory") || r.starts_with("no EngramDB") {
                    "no memories written to this group yet".to_string()
                } else {
                    r
                }
            }),
            parent_id: None,
            children: Vec::new(),
            worktree: false,
            subscriptions: Vec::new(),
            subscribers: subscribers
                .get(g.group_id.as_str())
                .cloned()
                .unwrap_or_default(),
            memory_count: f.memory_count,
            last_write_at: f.last_write_at,
            schema: f.schema,
        });
    }

    let global_root = paths::global_store_dir().map_err(ApiError::internal)?;
    let f = facts(&global_root, paths::global_lancedb_dir().ok()).await;
    ordered.push(ProjectView {
        id: paths::GLOBAL_PROJECT_ID.to_string(),
        name: "Global store".to_string(),
        kind: StoreKind::Global,
        path: global_root.display().to_string(),
        current: false,
        available: f.available,
        unavailable_reason: f.reason.map(|r| {
            if r.starts_with("the project directory") || r.starts_with("no EngramDB") {
                "no global memories yet".to_string()
            } else {
                r
            }
        }),
        parent_id: None,
        children: Vec::new(),
        worktree: false,
        subscriptions: Vec::new(),
        subscribers: Vec::new(),
        memory_count: f.memory_count,
        last_write_at: f.last_write_at,
        schema: f.schema,
    });

    Ok(Catalog {
        current: current.map(|c| c.id.clone()),
        projects: ordered,
    })
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
    if !paths::project_dir(&root).is_dir() {
        let reason = match &entry.parent_project_id {
            Some(p) if root.is_dir() => format!("sub-project: its memories live in {p}"),
            _ if !root.is_dir() => "the project directory no longer exists".into(),
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
