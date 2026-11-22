//! Store targets: resolving a tool's `project` argument to a directory or
//! an open store, and the gates (cross-project writes, all-projects
//! harvest, registry links) checked before any of it.

use super::*;

/// Refuse a memory-**store** target on a tool that reads the filesystem.
///
/// Harvest works off Claude Code transcripts, which are filed by working
/// directory. `global` and `group:<name>` name stores, not checkouts:
/// `resolve_dir` maps them to a directory holding no transcripts, so
/// `harvest_list` returns empty with no explanation and `harvest_mark` writes
/// a ledger nothing will ever read — the model believes a session is settled
/// while the project's own ledger is untouched. Silent wrong answers, so they
/// are refused by name rather than left to fail quietly.
///
/// Deliberately *not* aliased to `all_projects`: that is gated by
/// `[security] allow_all_projects_harvest`, and aliasing would route around
/// the gate.
pub(super) fn reject_store_target(project: Option<&str>) -> Result<(), String> {
    if StoreSpec::parse(project).is_shared() {
        return Err(error_response(
            ErrorCode::ValidationError,
            "The harvest tools operate on a filesystem project, not a memory store: \
             `global` and `group:<name>` hold no Claude Code transcripts, so there is nothing \
             there to list, digest, or mark. Pass an absolute project path or a 16-char project \
             ID, or omit `project` to use the current one.",
        ));
    }
    Ok(())
}

impl EngramDbServer {
    /// Resolve the target project directory from an optional project override.
    ///
    /// - `None`: returns the effective (hierarchy-resolved) dir for the
    ///   default project. In a linked git worktree this is the main
    ///   worktree's path.
    /// - `"global"`: returns the global store directory.
    /// - 16-char hex: looked up by project ID in the registry, then follows
    ///   `parent_project_id` links so worktree IDs transparently resolve to
    ///   their root project.
    /// - absolute path: canonicalized.  If the path is a linked worktree,
    ///   swaps to the main worktree's path.  The resulting project ID must
    ///   be present in the registry.
    pub(super) async fn resolve_dir(&self, project: Option<&str>) -> Result<PathBuf, String> {
        match StoreSpec::parse(project) {
            StoreSpec::Default => Ok(self.effective_dir.clone()),
            StoreSpec::Global => engramdb::storage::paths::global_store_dir()
                .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string())),
            // `group:<name>` targets a named group store (the multi-project
            // memories tier between one project and machine-global). The name
            // is normalized into a stable group id and mapped to its store
            // directory — `StoreSpec::parse` checks it before the 16-hex
            // project-id form so a group name can never be misread as a
            // project id.
            StoreSpec::Group { id, .. } => engramdb::storage::paths::group_store_dir(&id)
                .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string())),
            StoreSpec::ProjectId(input) => self.resolve_project_id(&input).await,
            StoreSpec::Path(path) => self.resolve_project_path(path).await,
        }
    }

    /// [`Self::resolve_dir`] for a 16-hex project id.
    async fn resolve_project_id(&self, input: &str) -> Result<PathBuf, String> {
        let registry = self
            .registry
            .load()
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        // Resolve parent chain so that passing a worktree's ID routes to
        // the main project's storage.
        let root_id = engramdb::storage::resolve_root_project_id(&registry, input);
        match registry.projects.iter().find(|e| e.project_id == root_id) {
            Some(e) => Ok(PathBuf::from(&e.project_path)),
            None => Err(error_response(
                ErrorCode::ProjectNotFound,
                &format!(
                    "Project ID '{}' not found in registry. Run `engramdb init` in the target project first.",
                    input
                ),
            )),
        }
    }

    /// [`Self::resolve_dir`] for a path.
    async fn resolve_project_path(&self, path: PathBuf) -> Result<PathBuf, String> {
        let input = path.display().to_string();
        if !path.is_absolute() {
            return Err(error_response(
                ErrorCode::ValidationError,
                "Project path must be absolute, not relative.",
            ));
        }
        let canonical = path.canonicalize().map_err(|e| {
            error_response(
                ErrorCode::ProjectNotFound,
                &format!("Cannot access directory '{}': {}.", input, e),
            )
        })?;
        // If the caller pointed at a linked worktree, swap to the main
        // worktree's project root so storage ops hit the right place.
        let effective =
            engramdb::storage::project_id::detect_worktree_main(&canonical).unwrap_or(canonical);
        let project_id = engramdb::storage::project_id::compute_project_id(&effective);
        let registry = self
            .registry
            .load()
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        if !registry.projects.iter().any(|e| e.project_id == project_id) {
            return Err(error_response(
                ErrorCode::ProjectNotFound,
                &format!(
                    "Project at '{}' (id: {}) not found in registry. Run `engramdb init` there first.",
                    input, project_id
                ),
            ));
        }
        Ok(effective)
    }

    /// Open a MemoryStore for the given project override, auto-initializing only for the default project.
    pub(super) async fn open_store_for(
        &self,
        project: Option<&str>,
    ) -> Result<MemoryStore, String> {
        // `global` and a `group:<name>` target open (creating if necessary)
        // the shared store; only a project store must already exist.
        let shared = match StoreSpec::parse(project) {
            StoreSpec::Global => Some(StoreTarget::Global),
            StoreSpec::Group { id, .. } => Some(StoreTarget::Group(id)),
            _ => None,
        };
        if let Some(target) = shared {
            return MemoryStore::open_target(&target, OpenMode::ReadWrite)
                .await
                .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()));
        }

        let dir = self.resolve_dir(project).await?;
        let engramdb_dir = dir.join(".engramdb");
        if !engramdb_dir.exists() {
            if project.is_some() {
                return Err(error_response(
                    ErrorCode::StoreNotInitialized,
                    &format!(
                        "Store not initialized at '{}'. Run `engramdb init` there first.",
                        dir.display()
                    ),
                ));
            }
            // Default project: auto-init.  For linked worktrees this targets
            // the main project's root (via effective_dir resolution above)
            // and also registers the worktree as a sub-project.
            MemoryStore::init(&dir, self.registry.as_ref())
                .await
                .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))?;
            if self.dir != self.effective_dir && dir == self.effective_dir {
                let child_id = engramdb::storage::project_id::compute_project_id(&self.dir);
                let parent_id =
                    engramdb::storage::project_id::compute_project_id(&self.effective_dir);
                self.registry
                    .update_with_parent(&self.dir, &child_id, Some(&parent_id))
                    .await
                    .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
            }
        }
        MemoryStore::open(&dir)
            .await
            .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))
    }

    /// Open a MemoryStore for the default project, auto-initializing if needed.
    pub(super) async fn open_store(&self) -> Result<MemoryStore, String> {
        self.open_store_for(None).await
    }

    pub(super) async fn load_config_for(
        &self,
        project: Option<&str>,
    ) -> Result<engramdb::types::EngramConfig, String> {
        let dir = self.resolve_dir(project).await?;
        let config_path = dir.join(".engramdb").join("config.toml");
        Ok(load_config_or_default(&config_path).await)
    }

    /// The user's `[security]` settings, from the **global** config file.
    ///
    /// Never from a project's `.engramdb/config.toml`, neither the target's
    /// nor the session's own. The target's made any one repository with the
    /// key set a universal opener, since the agent picks the target. The
    /// session's own is no better: that file is committed, so in a cloned
    /// repository the repository's author wrote it, and a permission a caller
    /// can grant itself is not a permission.
    async fn security_config(&self) -> engramdb::types::config::SecurityConfig {
        engramdb::storage::config::load_global_config_or_default()
            .await
            .security
    }

    /// Where the user sets `[security]`, for refusal messages.
    fn global_config_hint() -> String {
        engramdb::storage::paths::global_config_path()
            .map(|p| format!("the global config file {}", p.display()))
            .unwrap_or_else(|_| "the global config file".to_string())
    }

    /// Confused-deputy guard for the harvest tools' *read* scope.
    ///
    /// Two things, both keyed off the user's global config
    /// ([`Self::security_config`]), never a project's.
    ///
    /// 1. `all_projects` needs the opt-in.
    /// 2. So does naming a *different* project. Cross-project reads are
    ///    ordinarily fine for memories — those are curated. Transcripts are
    ///    the raw conversation, a materially more sensitive class, and
    ///    `allow_all_projects_harvest` is already exactly the flag that means
    ///    "this agent may read transcripts beyond its own project", so this
    ///    reuses it rather than inventing a second knob.
    pub(super) async fn check_harvest_scope(
        &self,
        project: Option<&str>,
        all_projects: Option<bool>,
    ) -> Result<bool, String> {
        let allowed = self.security_config().await.allow_all_projects_harvest;

        if all_projects == Some(true) && !allowed {
            return Err(error_response(
                ErrorCode::ValidationError,
                &format!(
                    "Machine-wide harvest is disabled by [security] \
                     allow_all_projects_harvest = false (the default). It reads every Claude \
                     Code transcript on this machine, including other projects' conversations. \
                     Only the user can enable it, in {}. A project's .engramdb/config.toml \
                     cannot.",
                    Self::global_config_hint()
                ),
            ));
        }

        if project.is_some() {
            let target = self.resolve_dir(project).await?;
            // Compare *project ids*, not paths. `resolve_dir(Some(..))`
            // canonicalizes but `resolve_dir(None)` returns `effective_dir`
            // verbatim, and `engramdb setup` writes `serve --dir .` — so a
            // raw path comparison rejects the caller's own project whenever
            // it names itself by absolute path, id, or through a symlink.
            // `compute_project_id` canonicalizes both sides, which is what
            // `check_cross_project_write` already does.
            let target_id = engramdb::storage::project_id::compute_project_id(&target);
            let own_id = engramdb::storage::project_id::compute_project_id(&self.effective_dir);
            if target_id != own_id && !allowed {
                return Err(error_response(
                    ErrorCode::ValidationError,
                    &format!(
                        "Reading another project's transcripts is disabled by [security] \
                         allow_all_projects_harvest = false (the default). Unlike memories, a \
                         transcript is the raw conversation. Only the user can enable it, in \
                         {}. A project's .engramdb/config.toml cannot.",
                        Self::global_config_hint()
                    ),
                ));
            }
        }
        Ok(all_projects.unwrap_or(false))
    }

    /// Confused-deputy guard for MCP mutating tools.
    ///
    /// Nearly every tool accepts an optional `project` override that
    /// [`Self::resolve_dir`] resolves to *any* project in the global registry,
    /// not just the session's own. A steered agent could therefore mutate a
    /// different registered project on the same machine. This gate — consulted
    /// at the top of every mutating handler — enforces the user's
    /// `[security].allow_cross_project_writes` policy, read from the global
    /// config ([`Self::security_config`]).
    ///
    /// Always `Ok` when:
    /// - `project` is `None` (the session's own project), or
    /// - `project` is `"global"` (the shared global store is the intended
    ///   cross-project store), or
    /// - `project` is `"group:<name>"` and the session's own project is
    ///   subscribed to that group (a member writing to its own group is not a
    ///   cross-project write), or
    /// - the override resolves to the session's *own* root project id (a
    ///   worktree of the session's own project is not cross-project), or
    /// - the global config allows cross-project writes (off by default).
    ///
    /// Otherwise (a different registered project, or a group the session is not
    /// a member of, with the gate off) returns a structured error and the write
    /// is refused.
    ///
    /// This authz is **registry-trust-level (advisory)**: group membership is
    /// read from the user-writable `registry.json`, the same untrusted-input
    /// trust surface as the existing project routing — it is not a hard security
    /// boundary.
    pub(super) async fn check_cross_project_write(
        &self,
        project: Option<&str>,
    ) -> Result<(), String> {
        // The session's own project and the shared global store are always
        // permitted, without touching the registry or config.
        let spec = StoreSpec::parse(project);
        match &spec {
            StoreSpec::Default | StoreSpec::Global => return Ok(()),
            _ => {}
        }

        // A `group:<name>` target is always allowed when the session's own
        // project is a member (subscribed) — mirroring how `"global"` (the
        // built-in everyone group) is always allowed. A non-member group write
        // falls through to the `allow_cross_project_writes` config gate below,
        // so it is gated rather than hard-blocked.
        if let StoreSpec::Group { name, id: gid } = &spec {
            let registry = self
                .registry
                .load()
                .await
                .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
            // The session's own root project id — computed the same way as the
            // `own_id` in the general branch below (effective_dir is already the
            // main worktree's path, so this is the root project).
            let own_id = engramdb::storage::project_id::compute_project_id(&self.effective_dir);
            if engramdb::storage::registry::subscriptions_of(&registry, &own_id)
                .iter()
                .any(|g| g == gid)
            {
                return Ok(());
            }
            // Non-member: fall through to the config gate (gated, not blocked).
            if self.security_config().await.allow_cross_project_writes {
                return Ok(());
            }
            return Err(error_response(
                ErrorCode::ValidationError,
                &format!(
                    "Cross-project writes are disabled by \
                     [security].allow_cross_project_writes = false (the default). This \
                     session's project (id: {own_id}) is not subscribed to group '{name}' \
                     (id: {gid}), so it may not write there. The user can subscribe with \
                     `engramdb groups subscribe {name}`, or enable cross-project writes in \
                     {}. Or omit the `project` parameter to write to your own project.",
                    Self::global_config_hint()
                ),
            ));
        }

        // Resolve the override to its project root exactly as `resolve_dir`
        // does, then compute the root id so a worktree of the target maps to
        // its main project (and a worktree of *our own* project maps to us).
        let target_dir = self.resolve_dir(project).await?;
        let target_id = engramdb::storage::project_id::compute_project_id(&target_dir);
        let own_id = engramdb::storage::project_id::compute_project_id(&self.effective_dir);
        if target_id == own_id {
            return Ok(());
        }

        // Different project: consult the user's global config.
        if self.security_config().await.allow_cross_project_writes {
            return Ok(());
        }

        Err(error_response(
            ErrorCode::ValidationError,
            &format!(
                "Cross-project writes are disabled by [security].allow_cross_project_writes = \
                 false (the default). This session's project (id: {own_id}) may not write to a \
                 different registered project (id: {target_id}). Omit the `project` parameter \
                 to write to your own project, or use project=\"global\" for the shared store. \
                 Only the user can enable cross-project writes, in {}. A project's \
                 .engramdb/config.toml cannot.",
                Self::global_config_hint()
            ),
        ))
    }

    /// Guard for the two registry writes an agent can make: `projects_link`
    /// and `projects_unlink`.
    ///
    /// A link is not bookkeeping. The harvest scope is "the root of the
    /// hierarchy plus every descendant", so linking another project under
    /// this one (or this one under another) hands this session that
    /// project's raw conversations through a plain `harvest_list`, with no
    /// `project` argument for the harvest gate to see. It also adopts that
    /// project's ledger and lets the two prune each other's transcript
    /// copies.
    ///
    /// `ids` are the projects the call names. One is "own" when its root in
    /// the registry is this session's root. All own: allowed (re-parenting
    /// inside one hierarchy changes no scope). Otherwise the global config
    /// must allow cross-project writes, and for a link (`widens_scope`) the
    /// machine-wide harvest read as well.
    pub(super) async fn check_registry_link(
        &self,
        ids: &[&str],
        widens_scope: bool,
    ) -> Result<(), String> {
        let registry = self
            .registry
            .load()
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        let own_id = engramdb::storage::project_id::compute_project_id(&self.effective_dir);
        let own_root = engramdb::storage::registry::resolve_root_project_id(&registry, &own_id);
        let foreign: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| {
                engramdb::storage::registry::resolve_root_project_id(&registry, id) != own_root
            })
            .collect();
        if foreign.is_empty() {
            return Ok(());
        }
        let security = self.security_config().await;
        let allowed = security.allow_cross_project_writes
            && (!widens_scope || security.allow_all_projects_harvest);
        if allowed {
            return Ok(());
        }
        let (needs, cli) = if widens_scope {
            (
                "[security] allow_cross_project_writes = true and allow_all_projects_harvest = \
                 true (linking shares past conversations between the two projects)",
                "engramdb projects link",
            )
        } else {
            (
                "[security] allow_cross_project_writes = true",
                "engramdb projects unlink",
            )
        };
        Err(error_response(
            ErrorCode::ValidationError,
            &format!(
                "Refused: project {} is outside this session's own project hierarchy (root id: \
                 {own_root}). Changing its links from an agent needs {needs} in {}; a project's \
                 .engramdb/config.toml cannot grant it. The user can run `{cli}` in a shell \
                 instead.",
                foreign.join(", "),
                Self::global_config_hint()
            ),
        ))
    }
}
