//! The startup work `run_stdio` defers past the MCP handshake, the gate
//! tool calls wait on, and the startup notices (embedding-model change,
//! auto-registration) it produces.

use super::*;

/// Progress of the startup work that `run_stdio` runs *after* the MCP
/// handshake instead of before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StartupStatus {
    Pending,
    Ready,
    /// Startup failed (for example the worktree's main project could not be
    /// initialized). Tool calls return this message instead of running
    /// against a half-set-up project.
    Failed(String),
}

impl StartupStatus {
    /// A receiver that is already `Ready`, for servers whose startup work is
    /// not deferred (tests, and the per-connection HTTP servers, whose startup
    /// runs once in `run_sse` before the listener binds).
    pub(super) fn ready_receiver() -> tokio::sync::watch::Receiver<StartupStatus> {
        tokio::sync::watch::channel(StartupStatus::Ready).1
    }
}

/// A startup notice for the agent, and whether the agent has seen it. Used
/// for the embedding-model-change warning and for the auto-registration
/// notice (see [`EngramDbServer::check_registration`]).
///
/// The embedding warning used to ride only in the `initialize` instructions. Since the
/// check that produces it now runs after `initialize` (it resolves the
/// embedding providers, which can take seconds on a cold start), it is often
/// not known yet when the instructions are sent. So whichever comes first
/// delivers it once: the instructions (when the check already finished) or
/// the first tool result after the check finished.
#[derive(Debug, Default)]
pub(super) struct StartupNotice {
    warning: std::sync::Mutex<Option<String>>,
    delivered: std::sync::atomic::AtomicBool,
}

impl StartupNotice {
    pub(super) fn new(warning: Option<String>) -> Self {
        Self {
            warning: std::sync::Mutex::new(warning),
            delivered: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(super) fn set(&self, warning: Option<String>) {
        *self.warning.lock().unwrap_or_else(|e| e.into_inner()) = warning;
    }

    pub(super) fn get(&self) -> Option<String> {
        self.warning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(super) fn mark_delivered(&self) {
        self.delivered
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// The warning, if there is one and nobody has delivered it yet. Marks it
    /// delivered.
    pub(super) fn take_undelivered(&self) -> Option<String> {
        let warning = self.get()?;
        let already = self
            .delivered
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        (!already).then_some(warning)
    }
}

/// The text the agent sees for an embedding-model-change warning.
pub(super) fn embedding_notice_text(warning: &str) -> String {
    format!("IMPORTANT — ACTION NEEDED: {warning} Tell the user.")
}

impl EngramDbServer {
    /// Evaluate the primary project's embedding-model identity for the
    /// startup warning. Best-effort: any failure or `off` mode → `None`
    /// (never blocks startup). Returns `(mode, report)`.
    async fn embedding_startup_report(
        &self,
    ) -> Option<(
        engramdb::types::ReindexOnModelChange,
        ops::EmbeddingModelReport,
    )> {
        // Best-effort, but never *silently* skipped: a transient I/O failure
        // resolving the project or opening the store would otherwise drop the
        // model-change warning with no trace at all. Log and bail instead.
        let dir = match self.resolve_dir(None).await {
            Ok(dir) => dir,
            Err(e) => {
                tracing::warn!(
                    "EngramDB: skipping embedding model-change check \
                     (cannot resolve project dir): {e}"
                );
                return None;
            }
        };
        let store = match self.open_store_for(None).await {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(
                    "EngramDB: skipping embedding model-change check \
                     (cannot open store): {e}"
                );
                return None;
            }
        };
        let config_path = dir.join(".engramdb").join("config.toml");
        let config = load_config_or_default(&config_path).await;
        let mode = config.embeddings.reindex_on_model_change;
        if mode == engramdb::types::ReindexOnModelChange::Off {
            return None;
        }
        let providers = Self::resolve_providers(
            &self.provider_cache,
            &self.daemon,
            self.embedding_backend,
            &config,
        )
        .await;
        let current =
            providers
                .embedding
                .as_ref()
                .map(|p| engramdb::storage::EmbeddingFingerprint {
                    model: p.model_id(),
                    dimensions: p.dimensions(),
                    composition: config.embeddings.composition_id(),
                });
        Some((mode, ops::embedding_model_report(&store, current).await))
    }

    /// The startup work for one server process: stats hydration, the
    /// worktree hierarchy, throttled housekeeping, the embedding-model-change
    /// check (with the `auto` reindex), then the background model warmup and
    /// daemon heartbeat.
    ///
    /// Slow on a cold start: the model check resolves the embedding
    /// providers, which spawns the shared daemon or loads ONNX in-process.
    /// `run_stdio` therefore runs this after the MCP handshake (see
    /// [`serve_with_deferred_startup`]); `run_sse` runs it before binding.
    pub(super) async fn run_startup(&self) -> anyhow::Result<()> {
        // Hydrate counters by replaying recent events from each project's
        // LanceDB `stats_events` table. Replay resets a project's counters,
        // which is one reason tool calls wait for startup to finish.
        if let Err(e) = engramdb::telemetry::persistence::hydrate_collector(&self.stats).await {
            tracing::warn!("stats hydrate failed: {e}");
        }
        // Detect git worktrees and register/init the main project if needed.
        self.ensure_hierarchy().await?;
        // A store whose registry entry is missing (a removable drive set up on
        // another computer, a reset registry) is registered again here.
        self.check_registration().await;
        // On the main worktree, run throttled housekeeping (orphan cleanup +
        // a quick store health check). Best-effort: never fails startup.
        self.maintain_main_project().await;
        self.check_embedding_model().await;
        // Load the embedding model in the background now so the first tool
        // call doesn't pay the ~240ms ONNX session init synchronously.
        self.spawn_provider_warmup();
        // Keep the shared daemon resident while this session runs and
        // self-heal it if it dies/restarts.
        self.spawn_daemon_heartbeat();
        Ok(())
    }

    /// Register the project if it has a store but the registry does not know
    /// it, and keep the notice for the agent. Best-effort: never fails
    /// startup.
    pub(super) async fn check_registration(&self) {
        match ops::projects::ensure_registered(&self.effective_dir, self.registry.as_ref()).await {
            Ok(check) => {
                if let Some(mut notice) = check.agent_notice() {
                    // A project that fell out of the registry may have had its
                    // data directory swept, leaving an empty index beside
                    // intact memory files. Startup is deferred past the
                    // handshake, so the rebuild can run here.
                    if let Ok(store) = MemoryStore::open(&self.effective_dir).await {
                        if let Some(rows) = ops::maintenance::rebuild_empty_index(&store).await {
                            notice.push_str(&format!(
                                " Its search index had been emptied, so EngramDB rebuilt it from \
                                 the {rows} memory file(s); run `engramdb doctor` to see whether \
                                 their vectors need `engramdb reindex --embeddings-only`."
                            ));
                        }
                    }
                    tracing::warn!("{notice}");
                    self.registration_notice.set(Some(notice));
                }
            }
            Err(e) => tracing::warn!("EngramDB: registry check failed (continuing): {e}"),
        }
    }

    /// Embedding-model-change check: warn (default), auto-reindex, or — in
    /// `error` mode — leave the warning so embedding tools hard-fail.
    ///
    /// In `auto` mode the warning is published only if the reindex fails, so
    /// the agent is never told about a mismatch that is already repaired.
    async fn check_embedding_model(&self) {
        let Some((mode, report)) = self.embedding_startup_report().await else {
            return;
        };
        if report.status.is_consistent() {
            return;
        }
        if let Some(w) = &report.warning {
            tracing::warn!("{w}");
        }
        if mode == engramdb::types::ReindexOnModelChange::Auto {
            tracing::warn!(
                "EngramDB: reindex_on_model_change=auto — re-embedding before serving tool calls…"
            );
            match self.auto_reindex_default().await {
                Ok(()) => {
                    tracing::warn!("EngramDB: auto-reindex complete.");
                    return;
                }
                Err(e) => tracing::warn!("EngramDB: auto-reindex failed: {e}"),
            }
        }
        self.embedding_notice.set(report.warning);
    }

    /// The server's `initialize` / `server/discover` info. The instructions
    /// carry `embedding_warning` when there is one.
    pub(super) fn server_config(
        &self,
        embedding_warning: Option<&str>,
        registration_notice: Option<&str>,
    ) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        // rmcp 1.x marks these result/info structs `#[non_exhaustive]`, so build
        // them via the provided constructors/builders instead of struct literals.
        let server_info = Implementation::new("engramdb", env!("CARGO_PKG_VERSION"));
        let instructions = {
            let mut s = "Project-scoped persistent memory store for coding agents. \
                 Stores decisions, hazards, conventions, and context about the codebase. \
                 IMPORTANT: Query memories (query) before answering project questions, \
                 investigating workflows, or researching how things work — not only before \
                 modifying files. Use mode=\"filter\" with a query/logical/path/tags signal \
                 for specific lookups, mode=\"rank\" for context-aware browsing. \
                 Store new knowledge after significant discoveries — for decisions, \
                 state the premise (premise) and what would invalidate them \
                 (invalidated_by); set epistemic (fact/observation/decision) when it \
                 differs from the type default, and origin_task + generality=\"task\" \
                 for task-specific choices. Declare situation on query when debugging \
                 or weighing a design choice. \
                 Save what cost you effort: when a command failed and finding the cause \
                 took more than one try, and nothing in the repo or the error message states \
                 that cause, create a hazard before you report back (the command, the \
                 symptom, the cause and the fix). Save it yourself rather than offering to, \
                 unless the user asked you not to save memories. Skip it when the docs or \
                 the error already said what to do, or when the fix was a change to the code. \
                 Memory tools accept an optional `project` parameter (absolute path, 16-char \
                 project ID, \"global\", or \"group:<name>\") to operate on a different \
                 store's memories; the harvest tools and projects_info accept a path or \
                 project ID only. \
                 Use project=\"global\" for cross-project memories like personal preferences, \
                 coding conventions, or knowledge that applies everywhere. \
                 Use project=\"group:<name>\" to write to or read a named group store shared \
                 by a set of related projects (set up on the CLI with `engramdb groups \
                 subscribe <name>`); a project's subscribed groups fan into its queries \
                 automatically. \
                 Use include_global=true on query to also merge the everyone/global store. \
                 Omit `project` to use the current project. \
                 When you finish the task you were assigned, reflect: if anything durable \
                 about the project, the environment/tooling, or the user's preferences came \
                 up (not task minutiae), query existing memories, then create the new ones \
                 and challenge contradictions. If a durable decision or convention applies \
                 to more than the current project — e.g. you are working across a set of \
                 related repos — save it once to a shared store (project=\"group:<name>\" \
                 for that set, or project=\"global\" for everywhere) instead of only the \
                 current project, so every relevant project surfaces it. Suggested, not required. \
                 Where to save: project facts, conventions, hazards and decisions go to this \
                 store with create, including when the user says \"remember\". Claude Code's \
                 file-based auto-memory is private to one machine and invisible to these \
                 queries and to collaborators; keep it for personal collaboration preferences. \
                 Challenge a memory as soon as a source states the opposite: the user saying \
                 it is outdated (even if the code still matches it), or a file you read: a \
                 dated doc, a dependency pin, existing code doing what the memory forbids. Do \
                 not wait for the user to confirm a file-based conflict. Challenge first, then \
                 work from the newer source and tell the user. Not contradictions: code that \
                 merely doesn't mention the rule, code the memory itself calls legacy or an \
                 exception, a narrower memory scoping an exception, a source that is clearly \
                 older than the memory (an undated source still counts), a one-off or \
                 hypothetical request, a memory already marked superseded."
                .to_string();
            if let Some(w) = embedding_warning {
                s.push_str("\n\n");
                s.push_str(&embedding_notice_text(w));
            }
            if let Some(n) = registration_notice {
                s.push_str("\n\n");
                s.push_str(n);
            }
            s
        };
        InitializeResult::new(capabilities)
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(server_info)
            .with_instructions(instructions)
    }

    /// Wait until the deferred startup work has finished. Returns its error if
    /// it failed, so a tool never runs against a half-set-up project.
    pub(super) async fn await_startup(&self) -> Result<(), rmcp::ErrorData> {
        let mut status = self.startup.clone();
        let finished = status
            .wait_for(|s| *s != StartupStatus::Pending)
            .await
            .map(|s| s.clone());
        match finished {
            Ok(StartupStatus::Failed(e)) => Err(rmcp::ErrorData::internal_error(
                format!("EngramDB startup failed: {e}"),
                None,
            )),
            Ok(_) => Ok(()),
            // The startup task ended without reporting (it was aborted or
            // panicked).
            Err(_) => Err(rmcp::ErrorData::internal_error(
                "EngramDB startup did not finish",
                None,
            )),
        }
    }

    /// Re-embed the primary project's memories and stamp the model
    /// fingerprint (used by `reindex_on_model_change = auto` at startup).
    async fn auto_reindex_default(&self) -> anyhow::Result<()> {
        let store = self
            .open_store_for(None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let engine = self
            .assemble_engine_for(None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        // `embeddings_only`, not `force`: this fires *because* the model
        // changed, and the model id is part of every embed digest — so every
        // memory's digest already mismatches and nothing is skipped. Passing
        // `force` would produce the identical result at the cost of making the
        // digest's correctness untested on the one path that most depends on
        // it.
        ops::reindex(
            &store,
            Some(&engine),
            ops::ReindexOptions {
                embeddings_only: true,
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }
}
