//! Which store a caller means, and how it opens it.
//!
//! Three tiers of store exist (a project, a named group, the machine-global
//! store) and every front end used to pick between their constructors with
//! its own string checks. This module names the choice once:
//!
//! - [`StoreSpec`] is what a caller *wrote* (`"global"`, `"group:<name>"`, a
//!   16-hex project id, a path, or nothing). Parsing it is pure: no registry
//!   read, no filesystem access. Turning a `ProjectId` or `Path` into a
//!   directory is the front end's job, because the rules differ (the MCP
//!   server follows worktree links and checks the registry; an edge's `store`
//!   field accepts ids only).
//! - [`StoreTarget`] is a resolved store: a project directory, the global
//!   store, or a group by its canonical id.
//! - [`OpenMode`] says whether the open may write.
//!
//! [`MemoryStore::open_target`] is the one `match` over the two; the named
//! constructors stay public and unchanged.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::paths;
use crate::store::MemoryStore;

/// A resolved store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreTarget {
    /// A project store rooted at this directory (the one holding `.engramdb/`).
    Project(PathBuf),
    /// The machine-global store.
    Global,
    /// A named group store, by its canonical group id
    /// ([`paths::compute_group_id`]), never by its name.
    Group(String),
}

/// Whether an open may write.
///
/// The two modes are deliberately asymmetric across tiers, exactly as the
/// named constructors are:
///
/// - `ReadWrite` on a **project** fails with
///   [`crate::StorageError::NotInitialized`] when `.engramdb/` is absent
///   (creating a project store is `MemoryStore::init`, which also writes the
///   registry and `.gitignore`). On the **global** and **group** tiers it
///   creates the store when absent. Every writable open may backfill files,
///   create the LanceDB directory and run the schema migration.
/// - `ReadOnly` writes nothing on any tier: a store that was never created is
///   `NotInitialized`, one never indexed on this machine is a `Validation`
///   error naming the remedy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    ReadWrite,
    ReadOnly,
}

/// A store as a caller spelled it, parsed without any IO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreSpec {
    /// No store named: the caller's default project.
    Default,
    /// `"global"`.
    Global,
    /// `"group:<name>"`: the name as written after the prefix (for messages)
    /// and the canonical group id it hashes to (for everything else).
    Group { name: String, id: String },
    /// Sixteen ASCII hex digits: a project id, as written (not lowercased).
    ProjectId(String),
    /// Anything else, taken as a path (absolute or not; the caller decides
    /// whether a relative one is acceptable).
    Path(PathBuf),
}

impl StoreSpec {
    /// Parse a store spec. The order is load-bearing: `group:` is tried
    /// before the 16-hex form, so a group whose name is 16 hex digits stays
    /// a group. Nothing is trimmed: `"global"` must be written exactly.
    pub fn parse(spec: Option<&str>) -> Self {
        let Some(spec) = spec else {
            return Self::Default;
        };
        if spec == "global" {
            return Self::Global;
        }
        if let Some(name) = spec.strip_prefix("group:") {
            return Self::Group {
                name: name.to_string(),
                id: paths::compute_group_id(name),
            };
        }
        if is_project_id(spec) {
            return Self::ProjectId(spec.to_string());
        }
        Self::Path(PathBuf::from(spec))
    }

    /// Whether this names a shared (global or group) store.
    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Global | Self::Group { .. })
    }
}

/// Sixteen ASCII hex digits, either case: the shape of a project id.
fn is_project_id(s: &str) -> bool {
    s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl MemoryStore {
    /// Open the store `target` names in `mode`. See [`OpenMode`] for what
    /// each mode does on each tier; this is exactly the named constructor
    /// for that pair.
    pub async fn open_target(target: &StoreTarget, mode: OpenMode) -> Result<Self> {
        match (target, mode) {
            (StoreTarget::Project(dir), OpenMode::ReadWrite) => Self::open(dir).await,
            (StoreTarget::Project(dir), OpenMode::ReadOnly) => Self::open_read_only(dir).await,
            (StoreTarget::Global, OpenMode::ReadWrite) => Self::open_global().await,
            (StoreTarget::Global, OpenMode::ReadOnly) => Self::open_global_read_only().await,
            (StoreTarget::Group(id), OpenMode::ReadWrite) => Self::open_group(id).await,
            (StoreTarget::Group(id), OpenMode::ReadOnly) => Self::open_group_read_only(id).await,
        }
    }
}

impl StoreTarget {
    /// The target for a project directory.
    pub fn project(dir: &Path) -> Self {
        Self::Project(dir.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::StorageError;
    use crate::registry::InMemoryRegistry;

    #[test]
    fn store_spec_parses_every_form() {
        assert_eq!(StoreSpec::parse(None), StoreSpec::Default);
        assert_eq!(StoreSpec::parse(Some("global")), StoreSpec::Global);
        assert_eq!(
            StoreSpec::parse(Some("group:Team A")),
            StoreSpec::Group {
                name: "Team A".to_string(),
                id: paths::compute_group_id("Team A"),
            }
        );
        assert_eq!(
            StoreSpec::parse(Some("0123456789abcDEF")),
            StoreSpec::ProjectId("0123456789abcDEF".to_string())
        );
        assert_eq!(
            StoreSpec::parse(Some("/abs/project")),
            StoreSpec::Path(PathBuf::from("/abs/project"))
        );
        // Not trimmed, not case-folded: these are paths, as they were.
        assert_eq!(
            StoreSpec::parse(Some(" global")),
            StoreSpec::Path(PathBuf::from(" global"))
        );
        assert_eq!(
            StoreSpec::parse(Some("Global")),
            StoreSpec::Path(PathBuf::from("Global"))
        );
        // Fifteen and seventeen hex digits are not project ids.
        assert!(matches!(
            StoreSpec::parse(Some("0123456789abcde")),
            StoreSpec::Path(_)
        ));
        assert!(matches!(
            StoreSpec::parse(Some("0123456789abcdef0")),
            StoreSpec::Path(_)
        ));
        // `group:` wins over the 16-hex form.
        assert_eq!(
            StoreSpec::parse(Some("group:0123456789abcdef")),
            StoreSpec::Group {
                name: "0123456789abcdef".to_string(),
                id: paths::compute_group_id("0123456789abcdef"),
            }
        );
        assert!(StoreSpec::parse(Some("global")).is_shared());
        assert!(StoreSpec::parse(Some("group:x")).is_shared());
        assert!(!StoreSpec::parse(None).is_shared());
    }

    #[tokio::test]
    async fn open_target_matches_the_named_constructors() {
        let _lock = crate::test_support::acquire_global_test_lock().await;

        // Project, both modes.
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = StoreTarget::project(tmp.path());
        for mode in [OpenMode::ReadWrite, OpenMode::ReadOnly] {
            let err = MemoryStore::open_target(&missing, mode)
                .await
                .err()
                .unwrap();
            assert!(
                matches!(err, StorageError::NotInitialized),
                "{mode:?}: {err}"
            );
        }
        let init = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        for mode in [OpenMode::ReadWrite, OpenMode::ReadOnly] {
            let s = MemoryStore::open_target(&missing, mode).await.unwrap();
            assert_eq!(s.project_id, init.project_id, "{mode:?}");
            assert_eq!(s.project_dir, init.project_dir, "{mode:?}");
        }

        // Global: read-only does not create it, read-write does.
        let err = MemoryStore::open_target(&StoreTarget::Global, OpenMode::ReadOnly)
            .await
            .err()
            .unwrap();
        assert!(matches!(err, StorageError::NotInitialized), "{err}");
        let rw = MemoryStore::open_target(&StoreTarget::Global, OpenMode::ReadWrite)
            .await
            .unwrap();
        let named = MemoryStore::open_global().await.unwrap();
        assert_eq!(rw.project_id, named.project_id);
        assert_eq!(rw.project_dir, named.project_dir);
        let ro = MemoryStore::open_target(&StoreTarget::Global, OpenMode::ReadOnly)
            .await
            .unwrap();
        assert_eq!(ro.project_id, paths::GLOBAL_PROJECT_ID);
        assert_eq!(ro.project_dir, named.project_dir);

        // Group: same asymmetry.
        let gid = paths::compute_group_id("open target test");
        let target = StoreTarget::Group(gid.clone());
        let err = MemoryStore::open_target(&target, OpenMode::ReadOnly)
            .await
            .err()
            .unwrap();
        assert!(matches!(err, StorageError::NotInitialized), "{err}");
        let rw = MemoryStore::open_target(&target, OpenMode::ReadWrite)
            .await
            .unwrap();
        let named = MemoryStore::open_group(&gid).await.unwrap();
        assert_eq!(rw.project_id, gid);
        assert_eq!(rw.project_dir, named.project_dir);
        let ro = MemoryStore::open_target(&target, OpenMode::ReadOnly)
            .await
            .unwrap();
        assert_eq!(ro.project_id, gid);
        assert_eq!(ro.project_dir, named.project_dir);
    }

    /// Every file and directory under `root`, relative, sorted.
    fn listing(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                out.push(p.strip_prefix(root).unwrap().to_path_buf());
                if p.is_dir() {
                    stack.push(p);
                }
            }
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn read_only_target_creates_nothing() {
        // A project with `.engramdb/` but no `lancedb/` on this machine (a
        // fresh clone): a read-only open is refused and changes nothing,
        // neither under the project nor under its data directory.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(paths::memories_dir(tmp.path())).unwrap();
        std::fs::write(paths::project_dir(tmp.path()).join("manifest.toml"), "").unwrap();
        let project_id = crate::project_id::compute_project_id(tmp.path());
        let data = paths::lancedb_dir(&project_id).unwrap();
        let data_root = data.parent().unwrap().to_path_buf();

        let before = listing(tmp.path());
        let data_before = data_root.exists();
        let err = MemoryStore::open_target(&StoreTarget::project(tmp.path()), OpenMode::ReadOnly)
            .await
            .err()
            .unwrap();
        assert!(matches!(err, StorageError::Validation(_)), "{err}");
        assert_eq!(listing(tmp.path()), before);
        assert_eq!(data_root.exists(), data_before);
        assert!(!data.exists());
    }
}
