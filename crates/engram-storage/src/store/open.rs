//! Opening and initializing stores: project, global and group, writable and read-only.

use super::*;

impl MemoryStore {
    /// Initialize a new EngramDB store in the given directory.
    ///
    /// Idempotent: re-running `init` on an already-initialized project is
    /// safe. Existing `manifest.toml` (created_at, stats, parent link,
    /// embedding fingerprint) and `config.toml` (user-customized provider /
    /// dimensions / NLI / rerank settings) are never overwritten; only
    /// missing pieces are created.
    pub async fn init(dir: &Path, registry: &dyn RegistryBackend) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        // Compute the project ID up front so concurrent inits serialize on
        // the per-project advisory write lock. The lock file lives under the
        // *global* data dir (`projects/<id>/write.lock`), and
        // `acquire_write_lock` creates that directory itself, so there is no
        // bootstrap-ordering problem with the project dirs created below.
        let project_id = project_id::compute_project_id(dir);
        let _lock = write_lock::acquire_write_lock(&project_id).await?;

        // Create directory structure (create_dir_all is idempotent)
        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(dir)).await?;

        // Create manifest.toml only if missing. Re-running `init` must not
        // reset created_at / stats / parent_project_id, and critically must
        // not drop the embedding fingerprint (that would flip the store to
        // Untracked and defeat the model-swap corruption guard).
        let manifest_path = engramdb_dir.join("manifest.toml");
        if !manifest_path.exists() {
            let project_name = dir
                .canonicalize()
                .unwrap_or_else(|_| dir.to_path_buf())
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed-project".to_string());
            let manifest = manifest::Manifest {
                project: project_name,
                ..Default::default()
            };
            manifest::save_manifest(&manifest_path, &manifest).await?;
        }

        write_state_gitignore(&engramdb_dir).await;

        // Create the placeholder config.toml only if missing — never clobber
        // a user-customized config (a dimensions change alone would make the
        // next open build LanceIndex against an incompatible chunks table).
        let config_path = engramdb_dir.join("config.toml");
        if !config_path.exists() {
            async_fs::write(
                &config_path,
                "# EngramDB configuration\n# See documentation for available settings\n",
            )
            .await?;
        }

        // Create global LanceDB directory
        let lance_path = paths::lancedb_dir(&project_id)?;
        paths::ensure_private_parent(&lance_path)?;
        async_fs::create_dir_all(&lance_path).await?;

        // Load config to get embedding dimensions
        let config = match load_config(&config_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Failed to load config from {}, using defaults: {}",
                    config_path.display(),
                    e
                );
                engram_types::EngramConfig::default()
            }
        };

        // Initialize LanceIndex with configured dimensions
        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(StorageError::index("init"))?;

        // Create personal directories
        async_fs::create_dir_all(paths::personal_memories_dir(&project_id)?).await?;

        // Update registry. `update` keeps the first registration: if another
        // still-existing checkout already owns this project ID (two clones of
        // the same remote), the entry continues to point at it.
        registry.update(dir, &project_id).await?;

        // Surface the shared-ID situation loudly at init time. Destructive
        // index operations are additionally guarded in `reindex`; `doctor`
        // reports it as a warning check.
        if let Ok(reg) = registry.load().await {
            if let Some(other) = super::registry::conflicting_checkout_path(&reg, &project_id, dir)
            {
                tracing::warn!(
                    "Project ID {} is already registered to another checkout at {} — \
                     both checkouts share one index; see `engramdb doctor`",
                    project_id,
                    other.display()
                );
            }
        }

        let store = Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        };
        // Release the init lock before migrating: the schema migration runs a
        // reindex, which re-acquires the per-project write lock (a second
        // acquire in this process would deadlock).
        drop(_lock);
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Initialize the global memory store.
    ///
    /// The global store lives under `<global_data_dir>/global/` and mirrors
    /// a normal project layout (`.engramdb/memories/`, `manifest.toml`, etc.)
    /// so all `MemoryStore` methods work unchanged.
    pub async fn init_global() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        let engramdb_dir = paths::project_dir(&global_dir);

        // Serialize lazy creation on the advisory write lock (as `init`/
        // `init_group` do): the everyone/global store is also created lazily on
        // first open, so concurrent first-writers must not race on table
        // creation.
        let _lock = write_lock::acquire_write_lock(paths::GLOBAL_PROJECT_ID).await?;

        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(&global_dir)).await?;

        // Create manifest
        let manifest_path = engramdb_dir.join("manifest.toml");
        if !manifest_path.exists() {
            let manifest = manifest::Manifest {
                project: "global".to_string(),
                ..Default::default()
            };
            manifest::save_manifest(&manifest_path, &manifest).await?;
        }

        // Create empty config if missing
        let config_path = engramdb_dir.join("config.toml");
        if !config_path.exists() {
            async_fs::write(
                &config_path,
                "# EngramDB global configuration\n# See documentation for available settings\n",
            )
            .await?;
        }

        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            global_dir,
            paths::GLOBAL_PROJECT_ID.to_string(),
            &paths::global_lancedb_dir()?,
            &config,
            "global init",
            Some(_lock),
        )
        .await
    }

    /// Open the global memory store, creating it if necessary.
    pub async fn open_global() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        let engramdb_dir = paths::project_dir(&global_dir);

        if !engramdb_dir.exists() {
            return Self::init_global().await;
        }

        let config_path = engramdb_dir.join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            global_dir,
            paths::GLOBAL_PROJECT_ID.to_string(),
            &paths::global_lancedb_dir()?,
            &config,
            "global open",
            None,
        )
        .await
    }

    /// Returns `true` if this store is the global memory store.
    pub fn is_global(&self) -> bool {
        self.project_id == paths::GLOBAL_PROJECT_ID
    }

    /// Initialize a named group memory store.
    ///
    /// A *group store* is the generalization of the global store: an ordinary
    /// machine-local store shared by a set of subscribed projects (see the
    /// multi-project-memories design). It lives under
    /// `<global_data_dir>/groups/<group_id>/` and mirrors a normal project
    /// layout so all `MemoryStore` methods work unchanged — this is a
    /// near-verbatim copy of [`init_global`](Self::init_global) rooted at the
    /// group paths, with `project_id = group_id` and a `group:<id>` manifest
    /// name so a group store is self-describing on disk.
    pub async fn init_group(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        let engramdb_dir = paths::project_dir(&group_dir);

        // Serialize lazy creation on the per-store advisory write lock, exactly
        // as `init` does. Group stores are created lazily on first open (from a
        // query fan-in or a `--group` write), so two concurrent multi-repo
        // sessions can race here on LanceDB table creation without it — the
        // loser would get a hard "table create failed". `acquire_write_lock`
        // creates its own lock dir, so there is no bootstrap-ordering problem.
        let _lock = write_lock::acquire_write_lock(group_id).await?;

        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(&group_dir)).await?;

        // Create manifest
        let manifest_path = engramdb_dir.join("manifest.toml");
        if !manifest_path.exists() {
            let manifest = manifest::Manifest {
                project: format!("group:{group_id}"),
                ..Default::default()
            };
            manifest::save_manifest(&manifest_path, &manifest).await?;
        }

        // Create empty config if missing
        let config_path = engramdb_dir.join("config.toml");
        if !config_path.exists() {
            async_fs::write(
                &config_path,
                "# EngramDB group configuration\n# See documentation for available settings\n",
            )
            .await?;
        }

        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            group_dir,
            group_id.to_string(),
            &paths::group_lancedb_dir(group_id)?,
            &config,
            "group init",
            Some(_lock),
        )
        .await
    }

    /// Open a named group memory store, creating it if necessary.
    pub async fn open_group(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        let engramdb_dir = paths::project_dir(&group_dir);

        if !engramdb_dir.exists() {
            return Self::init_group(group_id).await;
        }

        let config_path = engramdb_dir.join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            group_dir,
            group_id.to_string(),
            &paths::group_lancedb_dir(group_id)?,
            &config,
            "group open",
            None,
        )
        .await
    }

    /// Returns `true` if this store is a named group memory store.
    pub fn is_group(&self) -> bool {
        paths::is_group_id(&self.project_id)
    }

    /// Open an existing EngramDB store.
    pub async fn open(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Backfill on open, not just on init: every store created before the
        // `.gitignore` existed would otherwise keep `state/` tracked forever,
        // and nobody re-runs `init` on an existing project. `state/` now holds
        // the harvest ledger — session ids and free-text review notes — so
        // leaving it committable is exactly the leak the file exists to stop.
        // Cheap and idempotent: it returns immediately when the file is there.
        write_state_gitignore(&engramdb_dir).await;

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Load config to get embedding dimensions
        let config_path = engramdb_dir.join("config.toml");
        let config = match load_config(&config_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Failed to load config from {}, using defaults: {}",
                    config_path.display(),
                    e
                );
                engram_types::EngramConfig::default()
            }
        };

        // Open (or create) global LanceDB. Every open (the hot path for all
        // CLI commands and the MCP server) must migrate a store that predates
        // the current schema, exactly like the init/global paths — otherwise
        // reads that project the new columns fail and writes hit a schema
        // mismatch on an un-upgraded table. `open_writable_at` does both.
        let lance_path = paths::lancedb_dir(&project_id)?;
        Self::open_writable_at(
            dir.to_path_buf(),
            project_id,
            &lance_path,
            &config,
            "open",
            None,
        )
        .await
    }

    /// The tail every writable open but [`Self::init`] shares: create the
    /// LanceDB directory (owner-only parent), open the index at the
    /// configured width, then migrate the schema if it is behind.
    ///
    /// `lock` is the store's write lock when the caller holds it (the lazy
    /// creation paths). It is released after the index is open and before
    /// the migration, because the migration's reindex re-acquires it and a
    /// second acquire in this process would deadlock.
    ///
    /// The config is the caller's: [`Self::open`] and the shared-store opens
    /// load it with different fallbacks and warnings. `init` is not routed
    /// here because its registry write sits between the index open and the
    /// migration, and moving the index open after it would change what a
    /// failed init leaves behind.
    async fn open_writable_at(
        project_dir: PathBuf,
        project_id: String,
        lance_path: &Path,
        config: &engram_types::EngramConfig,
        what: &'static str,
        lock: Option<write_lock::WriteLockGuard>,
    ) -> Result<Self> {
        paths::ensure_private_parent(lance_path)?;
        async_fs::create_dir_all(lance_path).await?;
        let lance_index = LanceIndex::new(lance_path, config.embeddings.dimensions)
            .await
            .map_err(StorageError::index(what))?;
        let store = Self {
            project_dir,
            project_id,
            lance_index,
        };
        drop(lock);
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Open an existing project store for reading only, with no side effects.
    ///
    /// [`Self::open`] is not side-effect free, and must not be for its
    /// callers: it backfills `.engramdb/.gitignore`, creates the LanceDB
    /// directory and tables when missing, and runs the schema migration (a
    /// full reindex under the write lock). A long-running viewer (the
    /// dashboard) opening every store on the machine must do none of that —
    /// looking at a store is not a reason to rewrite it, and a migration run
    /// by a viewer would race the CLI the user is actually typing into. So
    /// this path writes nothing, creates no directory, table or lock, and a
    /// store behind the current schema is *reported* through
    /// [`Self::schema_state`], never fixed: reads that project a column the
    /// old table lacks fail on their own and say so.
    ///
    /// `NotInitialized` when `dir` has no `.engramdb/`; `Validation` naming
    /// the remedy when the store has never been indexed on this machine (a
    /// fresh clone with committed memories and no `lancedb/` yet).
    ///
    /// The returned value is still a full `MemoryStore`, so the type does not
    /// stop a caller from writing through it: a read-only consumer should
    /// wrap it in a type that exposes reads only (the dashboard does).
    pub async fn open_read_only(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);
        if !engramdb_dir.is_dir() {
            return Err(StorageError::NotInitialized);
        }
        let project_id = project_id::compute_project_id(dir);
        let lance_path = paths::lancedb_dir(&project_id)?;
        Self::open_read_only_at(dir.to_path_buf(), project_id, &lance_path).await
    }

    /// [`Self::open_read_only`] for the global store. `NotInitialized` when
    /// it was never created (unlike [`Self::open_global`], which creates it).
    pub async fn open_global_read_only() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        if !paths::project_dir(&global_dir).is_dir() {
            return Err(StorageError::NotInitialized);
        }
        let lance_path = paths::global_lancedb_dir()?;
        Self::open_read_only_at(
            global_dir,
            paths::GLOBAL_PROJECT_ID.to_string(),
            &lance_path,
        )
        .await
    }

    /// [`Self::open_read_only`] for a group store. `NotInitialized` when it
    /// was never created (unlike [`Self::open_group`], which creates it).
    pub async fn open_group_read_only(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        if !paths::project_dir(&group_dir).is_dir() {
            return Err(StorageError::NotInitialized);
        }
        let lance_path = paths::group_lancedb_dir(group_id)?;
        Self::open_read_only_at(group_dir, group_id.to_string(), &lance_path).await
    }

    async fn open_read_only_at(
        project_dir: PathBuf,
        project_id: String,
        lance_path: &Path,
    ) -> Result<Self> {
        let config_path = paths::project_dir(&project_dir).join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        let lance_index = LanceIndex::open_existing(lance_path, config.embeddings.dimensions)
            .await
            .map_err(StorageError::index("open"))?
            .ok_or_else(|| {
                StorageError::Validation(format!(
                    "{} has not been indexed on this machine yet: run any engramdb \
                     command there (or `engramdb reindex`) to build its index",
                    project_dir.display()
                ))
            })?;
        Ok(Self {
            project_dir,
            project_id,
            lance_index,
        })
    }

    /// Where this store's on-disk schema stands against this binary's, read
    /// from `manifest.toml` without changing anything — what
    /// [`Self::migrate_schema_if_needed`] would act on, for a caller that must
    /// only report it ([`Self::open_read_only`]).
    pub async fn schema_state(&self) -> SchemaState {
        Self::schema_state_at(&self.project_dir).await
    }

    /// [`Self::schema_state`] for the store rooted at `project_dir`, without
    /// opening it: a listing of every store on the machine reads this per row
    /// and must not connect to each LanceDB to do so.
    pub async fn schema_state_at(project_dir: &Path) -> SchemaState {
        let manifest_path = paths::project_dir(project_dir).join("manifest.toml");
        let loaded = manifest::load_manifest(&manifest_path).await.ok();
        let stored = loaded.as_ref().map(|m| m.schema_version.clone());
        let schema_stale = stored
            .as_deref()
            .is_none_or(|v| !manifest::schema_version_is_current(v));
        let normalizer_stale = loaded.as_ref().and_then(|m| m.normalizer.as_deref())
            != Some(engram_types::NORMALIZER_STAMP);
        SchemaState {
            stored,
            current: manifest::CURRENT_SCHEMA_VERSION,
            needs_migration: schema_stale || normalizer_stale,
        }
    }
}
