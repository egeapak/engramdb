//! Clap CLI application definitions.
//!
//! This module defines the command-line interface structure using Clap's derive macros.
//! It includes the main CLI struct, all subcommands, and output format options.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Subcommands for `engramdb hook`.
#[derive(Subcommand)]
pub enum HookCommand {
    /// Handle PreToolUse hook events (reads JSON from stdin, outputs additionalContext)
    PreToolUse,
    /// Handle SessionStart hook events (outputs high-criticality memories)
    SessionStart {
        /// Minimum criticality threshold for surfaced memories (0.0-1.0)
        #[arg(long, default_value = "0.6")]
        min_criticality: f64,
    },
    /// Handle UserPromptSubmit hook events (prompt-relevant memories with situation inference)
    UserPromptSubmit,
    /// Handle PostToolUse hook events (warn when an edit touches a memory's watch paths)
    PostToolUse,
    /// Handle SessionEnd hook events (housekeeping; no context output)
    SessionEnd,
    /// Handle PreCompact hook events (store-your-memories reminder before compaction)
    PreCompact,
    /// Catch-all for a hook event this binary does not know.
    ///
    /// The hook wiring in `.claude-plugin/plugin.json` and `settings.json` is
    /// installed independently of the binary (plugin marketplace update vs.
    /// `cargo install`), so a Claude Code config can name a `hook` subcommand
    /// that an older `engramdb` never shipped. Without this variant clap
    /// rejects the unknown name and exits 2 — which Claude Code treats as a
    /// *blocking* hook error, so a stale binary breaks every prompt in the
    /// session. Capturing it here routes the failure through the same
    /// fail-open contract the handlers already honor (see the backstop in
    /// `lib.rs` and `hook_all_subcommands_malformed_stdin_exit_zero`).
    ///
    /// Note that clap stops binding global args once it hands off to an
    /// external subcommand, so the trailing `--dir .` lands in this `Vec`
    /// rather than in `Cli::dir`. The handler only prints, so that is moot.
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

/// Subcommands for `engramdb task`.
#[derive(Subcommand)]
pub enum TaskCommand {
    /// Declare (or read, with no NAME) the task this session is working on
    Current {
        /// Task/feature name (short, human-readable)
        name: Option<String>,

        /// Session id (defaults to $CLAUDE_SESSION_ID / $MCP_SESSION_ID)
        #[arg(long = "session-id")]
        session_id: Option<String>,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },
    /// Mark a task finished: demote its task-scoped memories
    Complete {
        /// Task/feature name
        name: String,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },
}

/// Subcommands for `engramdb doctor`.
#[derive(Subcommand)]
pub enum DoctorCommand {
    /// Fast store health check (index consistency only)
    Store,
    /// Load each downloaded model and run a test inference to confirm it works
    Validate,
}

/// Subcommands for `engramdb daemon`.
#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Run the daemon event loop (this is what MCP auto-spawns).
    Run {
        /// Unix socket to bind. Defaults to the shared per-user path
        /// (also overridable via ENGRAMDB_DAEMON_SOCKET).
        #[arg(long)]
        socket: Option<PathBuf>,

        /// Seconds to stay alive with no active connections before exiting.
        #[arg(long)]
        idle_timeout: Option<u64>,
    },
    /// Show whether a daemon is running and its request metrics.
    Status {
        /// Socket to target. Overrides ENGRAMDB_DAEMON_SOCKET and config.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Ask a running daemon to exit gracefully.
    Stop {
        /// Socket to target. Overrides ENGRAMDB_DAEMON_SOCKET and config.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Stop a running daemon (if any) and start a fresh one.
    Restart {
        /// Socket to target. Overrides ENGRAMDB_DAEMON_SOCKET and config.
        #[arg(long)]
        socket: Option<PathBuf>,

        /// Idle timeout for the newly started daemon.
        #[arg(long)]
        idle_timeout: Option<u64>,
    },
}

/// Reject a blank `--memory` id at parse time.
///
/// `memories_created` is derived from how many ids were passed, so an empty
/// one made the ledger claim a memory that does not exist: `mark --memory ""`
/// reported "1 memory saved" and `ledger show` printed a blank line under
/// `Memories: 1`. Failing in clap names the flag and stops before anything is
/// written; `harvest_state::mark_harvested` refuses the same thing, which is
/// what covers the MCP `harvest_mark` tool.
fn parse_memory_id(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(
            "a memory id cannot be empty — omit --memory entirely when the session yielded \
             nothing, which still records the review"
                .to_string(),
        );
    }
    Ok(trimmed.to_string())
}

/// Subcommands for `engramdb harvest`.
#[derive(Subcommand)]
pub enum HarvestCommand {
    /// List past sessions in scope, newest activity first
    List {
        /// Only sessions active since this point: an RFC 3339 timestamp or a
        /// relative shorthand like `7d`, `12h`, `30m`, `2w`
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,

        /// Maximum number of sessions to list
        #[arg(long, short = 'n')]
        limit: Option<usize>,

        /// Also list sessions already recorded as harvested
        #[arg(long)]
        include_harvested: bool,

        /// Include sessions with no human turns
        #[arg(long)]
        include_empty: bool,

        /// Ignore project scoping and list every session on this machine
        #[arg(long)]
        all_projects: bool,

        /// Session id to omit (typically the caller's own, still being written)
        #[arg(long, value_name = "ID")]
        exclude_session: Option<String>,
    },

    /// Print a budgeted digest of one session
    Show {
        /// Session id, or a unique prefix of one
        session_id: String,

        /// Character budget for the digest. Defaults to `[harvest]
        /// digest_budget` (200000); `0` means unlimited.
        #[arg(long)]
        max_chars: Option<usize>,

        /// Include the assistant's reasoning blocks (verbose)
        #[arg(long)]
        include_thinking: bool,

        /// Include subagent turns (verbose; subagents report back into the
        /// main thread, so their raw turns are largely duplicate volume)
        #[arg(long)]
        include_sidechains: bool,

        /// Omit the tool-call trace, leaving prompts and prose only
        #[arg(long)]
        no_tools: bool,

        /// Search every session on this machine, not just this project's
        #[arg(long)]
        all_projects: bool,
    },

    /// Record that a session has been reviewed, so it is not offered again
    Mark {
        /// Session id, or a unique prefix of one
        session_id: String,

        /// Id of a memory created from this session (repeatable). Omit when
        /// the session yielded nothing — recording a zero-yield review is
        /// what stops it being re-read on the next harvest.
        #[arg(long = "memory", value_name = "ID", value_parser = parse_memory_id)]
        memory_ids: Vec<String>,

        /// Search every session on this machine, not just this project's.
        /// Mirrors `show`, so a session you were able to digest is always a
        /// session you can mark as reviewed.
        #[arg(long)]
        all_projects: bool,

        /// Record the session as deliberately postponed rather than settled.
        /// Deferred sessions keep appearing in `harvest list`.
        #[arg(long, conflicts_with = "memory_ids")]
        defer: bool,

        /// Why the session was skipped or deferred
        #[arg(long)]
        note: Option<String>,

        /// Curated one-or-two-sentence summary of what this session was
        /// about, written into the search index alongside the decision
        #[arg(long)]
        summary: Option<String>,
    },

    /// Index one or more sessions for `harvest search`
    Index {
        /// Session id, or a unique prefix of one. Omit with `--all`.
        session_id: Option<String>,

        /// Index every session in scope that still has bytes behind it
        #[arg(long, conflicts_with = "session_id")]
        all: bool,

        /// Re-embed even when the digest text is unchanged
        #[arg(long)]
        force: bool,
    },

    /// Search indexed past conversations
    Search {
        /// What to look for
        query: String,

        /// Maximum number of conversations to return
        #[arg(long, short = 'n', default_value_t = 10)]
        limit: usize,

        /// Only conversations that ended since this point: an RFC 3339
        /// timestamp or a relative shorthand like `30d`, `12h`, `2w`
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,

        /// Search every project's conversations on this machine
        #[arg(long)]
        all_projects: bool,
    },

    /// Set or replace a session's curated summary, re-embedding only it
    Summary {
        /// Session id, or a unique prefix of one
        session_id: String,

        /// The summary text. Pass an empty string to clear it.
        #[arg(conflicts_with_all = ["editor", "from_file"])]
        text: Option<String>,

        /// Compose the summary in $EDITOR
        #[arg(long, conflicts_with = "from_file")]
        editor: bool,

        /// Read the summary from a file (`-` for stdin)
        #[arg(long, value_name = "PATH")]
        from_file: Option<PathBuf>,
    },

    /// Forget a session's harvest record so it is offered again
    Reset {
        /// Session id, or a unique prefix of one
        session_id: String,
    },

    /// Inspect and manage the harvest ledger and its transcript archives
    Ledger {
        #[command(subcommand)]
        command: LedgerCommand,
    },
}

/// Subcommands for `engramdb harvest ledger`.
#[derive(Subcommand)]
pub enum LedgerCommand {
    /// List recorded sessions and their decisions
    List {
        /// Only entries with this decision (harvested, skipped, deferred,
        /// unreviewed)
        #[arg(long, value_name = "DECISION")]
        decision: Option<String>,

        /// Only entries at this stage (collected, indexed, compressed)
        #[arg(long, value_name = "STAGE")]
        stage: Option<String>,

        /// Only entries that still have an archived transcript
        #[arg(long)]
        with_archive: bool,
    },

    /// Show one entry in full, including archive metadata
    Show {
        /// Session id, or a unique prefix of one
        session_id: String,
    },

    /// Decompress an archived transcript back to a file
    Export {
        /// Session id, or a unique prefix of one
        session_id: String,

        /// Destination path (default: `<session-id>.jsonl` in the cwd)
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
    },

    /// Delete a ledger entry and/or its archive
    Rm {
        /// Session id, or a unique prefix of one
        session_id: String,

        /// Delete only the archived transcript, keeping the decision record
        #[arg(long)]
        archive_only: bool,

        /// Release the pin held by memories that cite this conversation, and
        /// delete the copy anyway
        #[arg(long)]
        unpin: bool,

        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,
    },

    /// Evict archives past the retention limits (dry run by default)
    Prune {
        /// Override `[harvest] archive_retention_days` for this run (days, e.g. `90d`)
        #[arg(long, value_name = "WHEN")]
        older_than: Option<String>,

        /// Override `[harvest] archive_max_bytes` for this run
        #[arg(long, value_name = "BYTES")]
        max_bytes: Option<u64>,

        /// Actually delete (default is a dry run, like `gc` and `compress`)
        #[arg(long)]
        apply: bool,
    },
}

/// Subcommands for `engramdb projects`.
#[derive(Subcommand)]
pub enum ProjectsCommand {
    /// Show info about the current project (default)
    Info,
    /// List all registered projects
    List {
        /// Directory-header grouping for this listing, overriding the
        /// `[cli].project_list_grouping` config: `auto` (header only for
        /// folders with 2+ projects), `always` (header per folder), or
        /// `none` (flat, full-path rows). Worktrees nest in every mode.
        #[arg(long, value_name = "MODE")]
        group: Option<engramdb::types::ProjectListGrouping>,
    },
    /// Walk a directory tree for `.engramdb/` projects the registry doesn't
    /// know about, then offer to register and index each one.
    ///
    /// Useful after cloning a repo that carries its memories, restoring from a
    /// backup, or losing `registry.json`: those projects exist on disk but are
    /// invisible to `projects list` and every cross-project surface until they
    /// are registered.
    Discover {
        /// Directory to scan (defaults to the current project directory)
        path: Option<std::path::PathBuf>,
        /// Maximum directory depth to descend below the scan root
        #[arg(long, default_value_t = engramdb::ops::discover::DEFAULT_MAX_DEPTH)]
        max_depth: usize,
        /// Also descend into hidden (dot-prefixed) directories
        #[arg(long)]
        hidden: bool,
        /// Follow directory symlinks while scanning
        #[arg(long)]
        follow_symlinks: bool,
        /// Register every discovered project without prompting (in JSON mode, required unless --dry-run)
        #[arg(long, short = 'y')]
        yes: bool,
        /// Report what would be registered and exit without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Register only — skip the index rebuild and re-embedding
        #[arg(long)]
        no_index: bool,
    },
    /// Re-key this project's registration to the ID it hashes to today.
    ///
    /// Adding a git remote after `engramdb init` changes the project's ID, so
    /// the registry keeps pointing at the old one: memories vanish from
    /// queries, group subscriptions detach, and personal memories become
    /// invisible. This migrates the registry entry (preserving subscriptions
    /// and worktree links), carries the personal memories across, and rebuilds
    /// the index.
    Repair {
        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,
        /// Re-key only — skip the index rebuild and re-embedding
        #[arg(long)]
        no_index: bool,
    },
    /// Remove a project from the registry and reclaim its index.
    ///
    /// Personal memories are KEPT unless `--purge` is passed: a project ID
    /// derived from a git remote is shared by every clone of that remote on
    /// this machine, and the registry records only one of them, so deleting
    /// one registration can destroy another checkout's only copy.
    ///
    /// Refuses by default when the project has sub-projects (children).
    /// Re-run with `--cascade` to also delete descendants, or unlink the
    /// children first with `engramdb projects unlink <id>`.
    Delete {
        /// Project ID to delete
        project_id: String,
        /// Skip confirmation prompt (required in JSON mode)
        #[arg(long, short = 'f')]
        force: bool,
        /// Also delete all descendants (children and their children).
        #[arg(long)]
        cascade: bool,
        /// Also delete personal memories. Without this they are kept: a
        /// remote-derived project ID is shared by every clone of that remote,
        /// and the registry cannot see the others.
        #[arg(long)]
        purge: bool,
    },
    /// Show aggregate statistics across all projects
    Stats,
    /// Remove stale registry entries and orphan data directories.
    ///
    /// Stale: projects registered but whose path no longer exists on disk.
    /// Orphan: data directories under the global store not tracked by the registry.
    Prune {
        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,
    },
    /// Link a project as a sub-project of another project.
    ///
    /// Memory operations on the child still target its own storage, but
    /// `engramdb projects list` displays the hierarchy, and a cascade delete
    /// of the parent will take the child with it.
    Link {
        /// Project ID of the child
        child: String,
        /// Project ID of the parent
        #[arg(long)]
        parent: String,
    },
    /// Remove the parent link on a project, promoting it back to a root project.
    Unlink {
        /// Project ID of the child
        project_id: String,
    },
}

/// Subcommands for `engramdb groups` (multi-project memory group stores).
///
/// A *group* is a named, machine-local memory store that a set of projects
/// subscribe to — the tier between one project and the machine-wide global
/// store. Membership lives in `registry.json` as per-project `subscriptions`.
#[derive(Subcommand)]
pub enum GroupsCommand {
    /// Create a named group store (idempotent; prints its group id).
    Create {
        /// Human-readable group name (case/whitespace-normalized into a stable id)
        name: String,
    },
    /// Subscribe the current project to a group (creating the group if needed).
    ///
    /// Subscribed groups fan into this project's queries by default, and the
    /// project may write to the group without tripping the cross-project gate.
    /// Prints the blast radius (group memory count + current subscribers) and
    /// confirms first; pair with --yes in non-interactive contexts.
    Subscribe {
        /// Group name to subscribe to
        name: String,
        /// Skip the blast-radius confirmation prompt (required in JSON mode)
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Unsubscribe the current project from a group (forgiving if not subscribed).
    ///
    /// Prints the blast radius (memories this project will stop seeing) and
    /// confirms first; pair with --yes in non-interactive contexts.
    Unsubscribe {
        /// Group name to unsubscribe from
        name: String,
        /// Skip the blast-radius confirmation prompt (required in JSON mode)
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// List all known groups and the current project's subscriptions.
    List,
    /// List the projects subscribed to a group (its blast radius) and how many
    /// memories the group holds.
    Members {
        /// Group name to inspect
        name: String,
    },
}

/// Output format for CLI commands.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    /// Human-friendly colored output with formatting
    Pretty,
    /// JSON output for programmatic parsing
    Json,
    /// Plain text output without colors
    Plain,
}

/// EngramDB command-line interface.
#[derive(Parser)]
#[command(
    name = "engramdb",
    about = "Project-scoped memory store for coding agents",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Output format
    // `--json` is a shorthand for `--format json`; rejecting the combination
    // avoids the silently-ignored `--json --format pretty` case (finding #18).
    #[arg(long, global = true, value_enum, conflicts_with = "json")]
    pub format: Option<OutputFormat>,

    /// Output as JSON
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential output
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Verbose output
    #[arg(long, short, global = true)]
    pub verbose: bool,

    /// No colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Working directory (default: current directory)
    #[arg(long, global = true)]
    pub dir: Option<PathBuf>,

    /// Embedding backend: auto, onnx, or ollama
    #[arg(long = "embedding-backend", global = true)]
    pub embedding_backend: Option<engramdb::types::EmbeddingBackend>,

    /// Force in-process model loading — never contact the shared embedding
    /// daemon. Equivalent to setting ENGRAMDB_IN_PROCESS=1.
    #[arg(long = "in-process", global = true)]
    pub in_process: bool,

    /// Spawn the shared embedding daemon if it is not already running, then
    /// route through it. By default the CLI only uses a daemon that is already
    /// running (connect-only).
    #[arg(long = "spawn-daemon", global = true)]
    pub spawn_daemon: bool,

    /// Skip the automatic main-worktree maintenance pass (orphan-project
    /// cleanup + a quick store health check) for this invocation. Equivalent
    /// to setting ENGRAMDB_DISABLE_AUTO_MAINTENANCE=1.
    #[arg(long = "no-maintenance", global = true)]
    pub no_maintenance: bool,
}

/// Available CLI commands.
#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new EngramDB store
    Init {
        /// Skip embedding model initialization
        #[arg(long)]
        no_embeddings: bool,

        /// Path to a config template file
        #[arg(long)]
        template: Option<PathBuf>,
    },

    /// Add a new memory
    Add {
        /// Type of memory
        #[arg(long, short = 't', value_name = "TYPE")]
        type_: Option<String>,

        /// Memory content
        #[arg(long, short = 'c')]
        content: Option<String>,

        /// Memory content as a trailing positional (alternative to --content).
        /// This is the form the Quick Start uses: `engramdb add -t decision -s "..." "content"`.
        #[arg(value_name = "CONTENT", conflicts_with = "content")]
        content_pos: Option<String>,

        /// Brief summary (required; prompted for in an interactive terminal)
        #[arg(long, short = 's')]
        summary: Option<String>,

        /// Short title (a few words) for human-readable filenames
        #[arg(long, short = 'T')]
        title: Option<String>,

        /// Physical scope (file paths or globs, can be repeated)
        #[arg(long, short = 'p')]
        physical: Vec<String>,

        /// Logical scope (dot-notation domains, can be repeated)
        #[arg(long, short = 'l')]
        logical: Vec<String>,

        /// Tags (comma-separated or repeated)
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Criticality score (0.0 to 1.0)
        #[arg(long)]
        criticality: Option<f64>,

        /// Confidence score (0.0 to 1.0)
        #[arg(long, default_value = "0.8")]
        confidence: f64,

        /// Extended details
        #[arg(long)]
        details: Option<String>,

        /// Visibility (shared or personal)
        #[arg(long)]
        visibility: Option<String>,

        /// Audience for a group/global share: project and/or group ids that may
        /// see this memory (comma-separated). Omit for whole-group visibility.
        /// Only meaningful with --group or --global.
        #[arg(long, value_delimiter = ',')]
        audience: Vec<String>,

        /// IDs of memories this one supersedes (comma-separated)
        #[arg(long)]
        supersedes: Option<String>,

        /// Epistemic class: fact, observation, or decision (defaults from type)
        #[arg(long)]
        epistemic: Option<String>,

        /// Premise this memory depends on (e.g. "while we pin ort rc.12")
        #[arg(long)]
        premise: Option<String>,

        /// Paths/globs whose change invalidates this memory (repeatable)
        #[arg(long = "invalidated-by")]
        invalidated_by: Vec<String>,

        /// Task/feature this memory was created for
        #[arg(long = "origin-task")]
        origin_task: Option<String>,

        /// Generality: project (default) or task
        #[arg(long)]
        generality: Option<String>,

        /// Valid-time start (RFC3339) — backdate when the claim became true
        #[arg(long = "valid-from")]
        valid_from: Option<String>,

        /// Decay strategy: none, linear, exponential, or step
        #[arg(long)]
        decay_strategy: Option<String>,

        /// Half-life in seconds for decay
        #[arg(long)]
        decay_half_life: Option<u64>,

        /// TTL in seconds for decay
        #[arg(long)]
        decay_ttl: Option<u64>,

        /// Minimum decay factor (0.0-1.0)
        #[arg(long)]
        decay_floor: Option<f64>,

        /// Launch interactive TUI prompts
        #[arg(long, short = 'i')]
        interactive: bool,

        /// Open $EDITOR for content entry
        #[arg(long, short = 'e')]
        editor: bool,

        /// Read details from file
        #[arg(long)]
        details_file: Option<PathBuf>,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,

        /// Write the memory into the named group store instead of the current
        /// project. Repo-relative physical scope is stripped on group writes
        /// (see the multi-project-memories design). Mutually exclusive with
        /// `--global`.
        #[arg(long, conflicts_with = "global")]
        group: Option<String>,
    },

    /// Get a memory by ID
    Get {
        /// Memory ID (supports prefix matching)
        id: String,

        /// Show complete details without truncation
        #[arg(long, short = 'f')]
        full: bool,

        /// Output the raw markdown file contents
        #[arg(long)]
        raw: bool,

        /// Print the memory's file path instead of content
        #[arg(long)]
        path: bool,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Query memories (unified ranked / filtered retrieval).
    ///
    /// `--mode rank` returns every memory passing the type/tag/criticality
    /// filters, sorted by composite score (use for context-aware browsing).
    /// `--mode filter` requires a positive relevance signal — at least one of
    /// `--query`, `--logical`, `--path`, or `--tags` must be set.
    Query {
        /// Retrieval mode. Required.
        #[arg(long, value_parser = ["rank", "filter"])]
        mode: String,

        /// Search query text (positional). Alternatively pass `--query`.
        #[arg(value_name = "QUERY")]
        query_pos: Option<String>,

        /// Search query text (explicit flag; overrides positional when both set).
        #[arg(long = "query")]
        query: Option<String>,

        /// Physical path context (scoring signal).
        #[arg(long, short = 'p')]
        path: Option<String>,

        /// Logical scope context — dot-notation; scoring signal, not a filter. Repeatable.
        #[arg(long, short = 'l')]
        logical: Vec<String>,

        /// Filter by type (repeatable).
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by tags (comma-separated or repeated).
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Minimum criticality threshold.
        #[arg(long)]
        min_criticality: Option<f64>,

        /// Maximum number of results.
        #[arg(long, short = 'n', default_value = "10")]
        max_results: usize,

        /// Detail level: summary, content, full.
        #[arg(long)]
        detail_level: Option<String>,

        /// Include expired memories.
        #[arg(long)]
        include_expired: bool,

        /// Filter by epistemic class: fact, observation, decision (repeatable).
        #[arg(long)]
        epistemic: Vec<String>,

        /// Your situation, to reweight classes: session_start, file_edit, debugging, design_choice.
        #[arg(long)]
        situation: Option<String>,

        /// Include invalidated memories (closed validity windows).
        #[arg(long = "include-invalidated")]
        include_invalidated: bool,

        /// Show relevance scores alongside results.
        #[arg(long)]
        show_scores: bool,

        /// Also merge global (cross-project) memories into the results.
        ///
        /// Runs the same query against the global store and folds its hits
        /// into the project results (deduplicated, re-sorted, truncated).
        /// Ignored when `--global` is set (already querying the global store).
        /// Note: groups this project is subscribed to already fan in
        /// automatically; this flag only adds the everyone/global store.
        #[arg(long)]
        include_global: bool,

        /// Query the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,

        /// Query a named group store directly (instead of the current project).
        /// Subscribed groups already fan into a normal project query; use this
        /// to inspect one group's memories in isolation.
        #[arg(long, conflicts_with = "global")]
        group: Option<String>,
    },

    /// List all memories
    List {
        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by epistemic class: fact, observation, decision (repeatable)
        #[arg(long)]
        epistemic: Vec<String>,

        /// Filter by tags (comma-separated or repeated)
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Filter by status
        #[arg(long, short = 's')]
        status: Option<String>,

        /// Filter by scope (matches physical or logical scopes)
        #[arg(long)]
        scope: Option<String>,

        /// Sort field: criticality (default), created, updated, type
        #[arg(long, default_value = "criticality")]
        sort: String,

        /// Reverse sort order
        #[arg(long, short = 'r')]
        reverse: bool,

        /// Maximum number of results to display
        #[arg(long, short = 'n')]
        limit: Option<usize>,

        /// Include invalidated memories (closed validity windows)
        #[arg(long = "include-invalidated")]
        include_invalidated: bool,

        /// List the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Update an existing memory
    Update {
        /// Memory ID (supports prefix matching)
        id: String,

        /// New type
        #[arg(long, short = 't')]
        type_: Option<String>,

        /// New content
        #[arg(long, short = 'c')]
        content: Option<String>,

        /// New summary
        #[arg(long, short = 's')]
        summary: Option<String>,

        /// New title (short, a few words, for human-readable filenames)
        #[arg(long, short = 'T')]
        title: Option<String>,

        /// New physical scope (replaces existing)
        #[arg(long, short = 'p')]
        physical: Vec<String>,

        /// New logical scope (replaces existing)
        #[arg(long, short = 'l')]
        logical: Vec<String>,

        /// New tags (comma-separated or repeated, replaces existing)
        // Replacing is mutually exclusive with incremental add/remove
        // (combining them is order-dependent and surprising) — finding #23.
        #[arg(long, value_delimiter = ',', conflicts_with_all = ["tags_add", "tags_remove"])]
        tags: Vec<String>,

        /// Tags to add (comma-separated)
        #[arg(long = "tags-add")]
        tags_add: Option<String>,

        /// Tags to remove (comma-separated)
        #[arg(long = "tags-remove")]
        tags_remove: Option<String>,

        /// New criticality
        #[arg(long)]
        criticality: Option<f64>,

        /// New confidence
        #[arg(long)]
        confidence: Option<f64>,

        /// New details
        #[arg(long)]
        details: Option<String>,

        /// Read details from file
        #[arg(long = "details-file")]
        details_file: Option<PathBuf>,

        /// New visibility
        #[arg(long)]
        visibility: Option<String>,

        /// New status
        #[arg(long)]
        status: Option<String>,

        /// IDs of memories this one supersedes (comma-separated)
        #[arg(long)]
        supersedes: Option<String>,

        /// Set the per-memory audience (project/group ids) for a group/global
        /// share (comma-separated). Only meaningful on a group/global memory.
        #[arg(long, value_delimiter = ',')]
        audience: Vec<String>,

        /// Clear the audience (whole-group visibility). Wins over --audience.
        #[arg(long)]
        clear_audience: bool,

        /// Epistemic class: fact, observation, or decision (defaults from type)
        #[arg(long)]
        epistemic: Option<String>,

        /// Premise this memory depends on (e.g. "while we pin ort rc.12")
        #[arg(long)]
        premise: Option<String>,

        /// Paths/globs whose change invalidates this memory (repeatable)
        #[arg(long = "invalidated-by")]
        invalidated_by: Vec<String>,

        /// Task/feature this memory was created for
        #[arg(long = "origin-task")]
        origin_task: Option<String>,

        /// Generality: project (default) or task
        #[arg(long)]
        generality: Option<String>,

        /// Valid-time start (RFC3339) — backdate when the claim became true
        #[arg(long = "valid-from")]
        valid_from: Option<String>,

        /// Clear the whole validity condition (premise/invalidated-by/origin-task/generality)
        #[arg(long = "clear-validity")]
        clear_validity: bool,

        /// Reopen a closed validity window (clears invalidated_at + superseded_by)
        #[arg(long = "clear-invalidated")]
        clear_invalidated: bool,

        /// Close the validity window now: the memory WAS true but no longer is.
        /// Preferred over delete — history stays queryable via --include-invalidated
        #[arg(long, conflicts_with = "clear_invalidated")]
        invalidate: bool,

        /// Id of the memory that supersedes this one (only with --invalidate)
        #[arg(long = "superseded-by", requires = "invalidate")]
        superseded_by: Option<String>,

        /// Decay strategy: none, linear, exponential, or step
        #[arg(long)]
        decay_strategy: Option<String>,

        /// Half-life in seconds for decay
        #[arg(long)]
        decay_half_life: Option<u64>,

        /// TTL in seconds for decay
        #[arg(long)]
        decay_ttl: Option<u64>,

        /// Minimum decay factor (0.0-1.0)
        #[arg(long)]
        decay_floor: Option<f64>,

        /// Open memory file in $EDITOR
        #[arg(long, short = 'e')]
        editor: bool,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,

        /// Update a memory in a named group store (instead of the current project)
        #[arg(long, conflicts_with = "global")]
        group: Option<String>,
    },

    /// Delete a memory
    Delete {
        /// Memory ID (supports prefix matching)
        id: String,

        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Task lifecycle: declare or complete the task this session works on
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },

    /// Confirm a memory is still accurate (stamps verified_at; clears
    /// doctor-flagged needs_review)
    Verify {
        /// Memory ID (supports prefix matching)
        id: String,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Show effective config values, thresholds, and the store's top tags
    Config {
        /// Number of top tags to show (most-used first)
        #[arg(long)]
        top_tags: Option<usize>,

        /// Show config for the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Show statistics
    Stats {
        /// Include the cross-project telemetry breakdown.
        #[arg(long)]
        all_projects: bool,

        /// Show statistics for the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,

        /// Show the shared embedding daemon's metrics instead of memory-store stats
        #[arg(long)]
        daemon: bool,
    },

    /// Check environment and store health
    Doctor {
        #[command(subcommand)]
        command: Option<DoctorCommand>,

        /// Check the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,

        /// Offer to fix detected issues (reindex, download model, prune registry, repair a
        /// drifted project ID, init).
        /// Prompts on a terminal; in non-interactive contexts pair with --yes.
        #[arg(long)]
        fix: bool,

        /// Apply fixes without prompting (use with --fix; required to fix in non-TTY contexts)
        #[arg(long)]
        yes: bool,
    },

    /// Manage registered EngramDB projects
    Projects {
        #[command(subcommand)]
        command: Option<ProjectsCommand>,
    },

    /// Inspect past Claude Code sessions for knowledge worth remembering
    ///
    /// Provides the raw material for the `/engram:harvest` slash command:
    /// `list` shows which sessions are in scope, `show` prints a budgeted
    /// digest of one, `mark` records that a session has been reviewed so it is
    /// not offered again, and `reset` undoes that record.
    ///
    /// `search` answers "did we ever discuss X" over past conversations,
    /// `index` builds the rows it reads (otherwise built by the maintenance
    /// pass), and `summary` attaches a curated description of a session to its
    /// row. `ledger` inspects and manages what has accumulated — the review
    /// decisions and the compressed transcript copies the SessionEnd hook
    /// keeps (`list`, `show`, `export`, `rm`, `prune`).
    ///
    /// Scope defaults to the root of this project's hierarchy plus every
    /// project registered under it — so from a git worktree that is the main
    /// checkout and its sibling worktrees too, since they share one memory
    /// store while filing transcripts under their own paths.
    Harvest {
        #[command(subcommand)]
        command: HarvestCommand,
    },

    /// Manage multi-project memory groups and this project's subscriptions
    Groups {
        #[command(subcommand)]
        command: GroupsCommand,
    },

    /// Challenge a memory's validity
    Challenge {
        /// Memory ID (supports prefix matching)
        id: String,

        /// Evidence or reason for the challenge
        #[arg(long, short = 'e')]
        evidence: String,

        /// Source file that contradicts this memory
        #[arg(long)]
        source_file: Option<String>,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Run garbage collection on low-relevance memories
    Gc {
        /// Actually delete (default is dry-run)
        #[arg(long)]
        confirm: bool,

        /// Score threshold for GC (default from config)
        #[arg(long)]
        threshold: Option<f64>,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// List compression candidates (actual compression requires MCP mode)
    Compress {
        /// Filter by logical scope
        #[arg(long)]
        scope: Option<String>,

        /// Criticality threshold for candidates (default 0.4)
        #[arg(long)]
        threshold: Option<f64>,

        /// Operate on the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Start the MCP server
    Serve {
        /// Transport type (stdio or sse)
        #[arg(long, default_value = "stdio")]
        transport: String,

        /// Port for SSE transport
        #[arg(long)]
        port: Option<u16>,
    },

    /// Run the shared embedding daemon (normally auto-spawned by MCP)
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },

    /// Generate shell completions
    Completions {
        /// Shell type
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Migrate memory files to the latest format version
    Migrate {
        /// Only report what would be migrated, don't change files
        #[arg(long)]
        dry_run: bool,

        /// Migrate the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Roll back memory files to a previous format version
    Rollback {
        /// Target format version (e.g., 1 for legacy YAML format). Defaults to 1.
        #[arg(long, default_value = "1")]
        target_version: u32,

        /// Only report what would be rolled back, don't change files
        #[arg(long)]
        dry_run: bool,

        /// Roll back the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Rebuild index and re-embed memories
    Reindex {
        /// Only re-embed, don't rebuild index
        #[arg(long, conflicts_with = "index_only")]
        embeddings_only: bool,

        /// Only rebuild index, don't re-embed
        #[arg(long)]
        index_only: bool,

        /// Rebuild the conversation search rows from the stored transcript
        /// copies instead of touching memories. Mirrors `--embeddings-only`:
        /// a rebuild of one index, from bytes that were kept verbatim so a
        /// better reduction is always a re-derivation away.
        #[arg(long, conflicts_with_all = ["embeddings_only", "index_only", "global"])]
        archive_only: bool,

        /// Reindex the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Claude Code plugin hook handler
    Hook {
        /// `None` when invoked as a bare `engramdb hook`. Optional rather than
        /// required so that case exits 0 through the same fail-open path as
        /// [`HookCommand::Unknown`], instead of clap's exit-2 usage error.
        #[command(subcommand)]
        command: Option<HookCommand>,
    },

    /// Set up Claude Code integration (hooks, MCP, ENGRAM.md, CLAUDE.md)
    Setup {
        /// Skip plugin install in global mode, write hooks and MCP directly to settings.json
        #[arg(long)]
        no_plugin: bool,

        /// Install to ~/.claude/ instead of project-local .claude/
        #[arg(long)]
        global: bool,

        /// Show what would be changed without writing
        #[arg(long)]
        dry_run: bool,

        /// Override .claude directory path (for testing)
        #[arg(long, hide = true)]
        claude_dir: Option<PathBuf>,
    },

    /// Interactive review of challenged/stale memories
    Review {
        /// Filter by logical scope
        #[arg(long)]
        scope: Option<String>,

        /// Filter by memory type
        #[arg(long, short = 't')]
        type_: Option<String>,

        /// Only show Status::Challenged memories
        #[arg(long)]
        challenged_only: bool,

        /// Only show Status::NeedsReview memories
        #[arg(long)]
        stale_only: bool,

        /// Recency trigger: also surface active memories not updated in more than
        /// N days. Bare `--stale-after-days` uses the 90-day default; omit the
        /// flag entirely to review only flagged memories.
        #[arg(long, num_args = 0..=1, default_missing_value = "90")]
        stale_after_days: Option<u64>,

        /// Review the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Epistemic-feature flag wiring: every new long flag must parse and land
    /// in the right field — a clap long-name typo ships silently otherwise.
    #[test]
    fn test_epistemic_flags_parse() {
        // add: all six epistemic fields.
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "content",
            "-s",
            "sum",
            "-t",
            "decision",
            "--epistemic",
            "decision",
            "--premise",
            "while rc12 is pinned",
            "--invalidated-by",
            "Cargo.lock",
            "--origin-task",
            "feat-x",
            "--generality",
            "task",
            "--valid-from",
            "2026-07-01T00:00:00Z",
        ])
        .expect("add epistemic flags must parse");
        match cli.command {
            Command::Add {
                epistemic,
                premise,
                invalidated_by,
                origin_task,
                generality,
                valid_from,
                ..
            } => {
                assert_eq!(epistemic.as_deref(), Some("decision"));
                assert_eq!(premise.as_deref(), Some("while rc12 is pinned"));
                assert_eq!(invalidated_by, vec!["Cargo.lock"]);
                assert_eq!(origin_task.as_deref(), Some("feat-x"));
                assert_eq!(generality.as_deref(), Some("task"));
                assert_eq!(valid_from.as_deref(), Some("2026-07-01T00:00:00Z"));
            }
            _ => panic!("expected Add"),
        }

        // update: clear flags + invalidate + superseded-by.
        let cli = Cli::try_parse_from([
            "engramdb",
            "update",
            "some-id",
            "--clear-validity",
            "--invalidate",
            "--superseded-by",
            "other-id",
        ])
        .expect("update invalidate flags must parse");
        match cli.command {
            Command::Update {
                clear_validity,
                clear_invalidated,
                invalidate,
                superseded_by,
                ..
            } => {
                assert!(clear_validity);
                assert!(!clear_invalidated);
                assert!(invalidate);
                assert_eq!(superseded_by.as_deref(), Some("other-id"));
            }
            _ => panic!("expected Update"),
        }
        // --superseded-by requires --invalidate; --invalidate conflicts with
        // --clear-invalidated.
        assert!(Cli::try_parse_from(["engramdb", "update", "id", "--superseded-by", "x"]).is_err());
        assert!(Cli::try_parse_from([
            "engramdb",
            "update",
            "id",
            "--invalidate",
            "--clear-invalidated"
        ])
        .is_err());

        // query: situation + epistemic filter + include-invalidated.
        let cli = Cli::try_parse_from([
            "engramdb",
            "query",
            "--mode",
            "filter",
            "--query",
            "x",
            "--situation",
            "debugging",
            "--epistemic",
            "observation",
            "--include-invalidated",
        ])
        .expect("query epistemic flags must parse");
        match cli.command {
            Command::Query {
                situation,
                epistemic,
                include_invalidated,
                ..
            } => {
                assert_eq!(situation.as_deref(), Some("debugging"));
                assert_eq!(epistemic, vec!["observation"]);
                assert!(include_invalidated);
            }
            _ => panic!("expected Query"),
        }

        // list: epistemic filter + include-invalidated.
        let cli = Cli::try_parse_from([
            "engramdb",
            "list",
            "--epistemic",
            "fact",
            "--epistemic",
            "decision",
            "--include-invalidated",
        ])
        .expect("list epistemic flags must parse");
        match cli.command {
            Command::List {
                epistemic,
                include_invalidated,
                ..
            } => {
                assert_eq!(epistemic, vec!["fact", "decision"]);
                assert!(include_invalidated);
            }
            _ => panic!("expected List"),
        }

        // verify + task subcommands.
        assert!(Cli::try_parse_from(["engramdb", "verify", "some-id"]).is_ok());
        assert!(Cli::try_parse_from([
            "engramdb",
            "task",
            "current",
            "feat-x",
            "--session-id",
            "s1"
        ])
        .is_ok());
        assert!(Cli::try_parse_from(["engramdb", "task", "current", "--global"]).is_ok());
        assert!(Cli::try_parse_from(["engramdb", "task", "complete", "feat-x"]).is_ok());
    }

    #[test]
    fn test_retrieve_with_query_long_flag() {
        // Test that `query --mode rank --query test` works without conflicting
        // with the global `-q` (quiet) flag.
        let result =
            Cli::try_parse_from(["engramdb", "query", "--mode", "rank", "--query", "test"]);
        assert!(
            result.is_ok(),
            "Failed to parse query --mode rank --query test: {:?}",
            result.err()
        );

        let cli = result.unwrap();
        match cli.command {
            Command::Query {
                mode,
                query,
                query_pos,
                ..
            } => {
                assert_eq!(mode, "rank");
                let text = query.or(query_pos);
                assert_eq!(text, Some("test".to_string()));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_completions_command_works() {
        // Test that completions bash works (this also previously panicked from -q conflict)
        let result = Cli::try_parse_from(["engramdb", "completions", "bash"]);
        assert!(
            result.is_ok(),
            "Failed to parse completions bash: {:?}",
            result.err()
        );

        let cli = result.unwrap();
        match cli.command {
            Command::Completions { shell } => {
                assert_eq!(shell, clap_complete::Shell::Bash);
            }
            _ => panic!("Expected Completions command"),
        }
    }

    #[test]
    fn test_quiet_flag_is_global() {
        let cli = Cli::try_parse_from(["engramdb", "-q", "list"]).unwrap();
        assert!(cli.quiet);
    }

    #[test]
    fn test_verbose_flag_is_global() {
        let cli = Cli::try_parse_from(["engramdb", "-v", "list"]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn test_format_flag_is_global() {
        let cli = Cli::try_parse_from(["engramdb", "--format", "json", "list"]).unwrap();
        match cli.format {
            Some(OutputFormat::Json) => {} // expected
            other => panic!("Expected Json, got {:?}", other),
        }
    }

    // Finding #18: `--json` and `--format` are mutually exclusive (so a
    // silently-ignored `--json --format pretty` is rejected instead).
    #[test]
    fn json_and_format_conflict() {
        // POSITIVE: each alone still parses.
        assert!(Cli::try_parse_from(["engramdb", "--json", "list"]).is_ok());
        assert!(Cli::try_parse_from(["engramdb", "--format", "pretty", "list"]).is_ok());
        // NEGATIVE (red before fix): together they must be rejected.
        assert!(
            Cli::try_parse_from(["engramdb", "--json", "--format", "pretty", "list"]).is_err(),
            "--json and --format must conflict"
        );
    }

    // Finding #23: incompatible flag combinations are rejected at parse time.
    #[test]
    fn reindex_embeddings_only_and_index_only_conflict() {
        assert!(Cli::try_parse_from(["engramdb", "reindex", "--embeddings-only"]).is_ok());
        assert!(Cli::try_parse_from(["engramdb", "reindex", "--index-only"]).is_ok());
        assert!(
            Cli::try_parse_from(["engramdb", "reindex", "--embeddings-only", "--index-only"])
                .is_err(),
            "--embeddings-only and --index-only must conflict"
        );
    }

    #[test]
    fn update_tags_replace_conflicts_with_add_remove() {
        assert!(Cli::try_parse_from(["engramdb", "update", "id", "--tags", "a,b"]).is_ok());
        assert!(Cli::try_parse_from(["engramdb", "update", "id", "--tags-add", "a"]).is_ok());
        assert!(
            Cli::try_parse_from(["engramdb", "update", "id", "--tags", "a", "--tags-add", "b"])
                .is_err(),
            "--tags (replace) must conflict with --tags-add"
        );
    }

    #[test]
    fn test_search_command_parses() {
        let cli =
            Cli::try_parse_from(["engramdb", "query", "--mode", "filter", "test query"]).unwrap();
        match cli.command {
            Command::Query {
                mode,
                query,
                query_pos,
                ..
            } => {
                assert_eq!(mode, "filter");
                let text = query.or(query_pos);
                assert_eq!(text, Some("test query".to_string()));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_add_command_all_flags() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--criticality",
            "0.5",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                type_,
                content,
                summary,
                criticality,
                ..
            } => {
                assert_eq!(type_, Some("decision".to_string()));
                assert_eq!(content, Some("content".to_string()));
                assert_eq!(summary, Some("summary".to_string()));
                assert_eq!(criticality, Some(0.5));
            }
            _ => panic!("Expected Add command"),
        }
    }

    /// The Quick Start / README form: content as a trailing positional
    /// (`engramdb add -t decision -s "..." "content"`). Locks the documented
    /// examples to the parser so doc drift fails tests instead of users.
    #[test]
    fn test_add_trailing_positional_content_parses() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-s",
            "Use rustls",
            "We chose rustls over openssl for static builds",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                content,
                content_pos,
                ..
            } => {
                assert_eq!(content, None);
                assert_eq!(
                    content_pos,
                    Some("We chose rustls over openssl for static builds".to_string())
                );
            }
            _ => panic!("Expected Add command"),
        }

        // --content and the positional are mutually exclusive.
        assert!(
            Cli::try_parse_from(["engramdb", "add", "-t", "decision", "-c", "x", "y"]).is_err()
        );
    }

    #[test]
    fn test_gc_command_flags() {
        let cli =
            Cli::try_parse_from(["engramdb", "gc", "--confirm", "--threshold", "0.1"]).unwrap();
        match cli.command {
            Command::Gc {
                confirm, threshold, ..
            } => {
                assert!(confirm);
                assert_eq!(threshold, Some(0.1));
            }
            _ => panic!("Expected Gc command"),
        }
    }

    #[test]
    fn test_serve_command_defaults() {
        let cli = Cli::try_parse_from(["engramdb", "serve"]).unwrap();
        match cli.command {
            Command::Serve { transport, port } => {
                assert_eq!(transport, "stdio");
                assert_eq!(port, None);
            }
            _ => panic!("Expected Serve command"),
        }
    }

    #[test]
    fn test_daemon_run_command_parsing() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "daemon",
            "run",
            "--socket",
            "/tmp/x.sock",
            "--idle-timeout",
            "42",
        ])
        .unwrap();
        match cli.command {
            Command::Daemon {
                command:
                    DaemonCommand::Run {
                        socket,
                        idle_timeout,
                    },
            } => {
                assert_eq!(socket, Some(PathBuf::from("/tmp/x.sock")));
                assert_eq!(idle_timeout, Some(42));
            }
            _ => panic!("Expected Daemon Run subcommand"),
        }
    }

    #[test]
    fn test_daemon_status_stop_restart_parsing() {
        // Bare status.
        match Cli::try_parse_from(["engramdb", "daemon", "status"])
            .unwrap()
            .command
        {
            Command::Daemon {
                command: DaemonCommand::Status { socket },
            } => assert_eq!(socket, None),
            _ => panic!("Expected Daemon Status"),
        }
        // Status with --socket override.
        match Cli::try_parse_from(["engramdb", "daemon", "status", "--socket", "/s.sock"])
            .unwrap()
            .command
        {
            Command::Daemon {
                command: DaemonCommand::Status { socket },
            } => assert_eq!(socket, Some(PathBuf::from("/s.sock"))),
            _ => panic!("Expected Daemon Status --socket"),
        }
        // Stop.
        match Cli::try_parse_from(["engramdb", "daemon", "stop"])
            .unwrap()
            .command
        {
            Command::Daemon {
                command: DaemonCommand::Stop { socket },
            } => assert_eq!(socket, None),
            _ => panic!("Expected Daemon Stop"),
        }
        // Restart with both options.
        match Cli::try_parse_from([
            "engramdb",
            "daemon",
            "restart",
            "--socket",
            "/r.sock",
            "--idle-timeout",
            "7",
        ])
        .unwrap()
        .command
        {
            Command::Daemon {
                command:
                    DaemonCommand::Restart {
                        socket,
                        idle_timeout,
                    },
            } => {
                assert_eq!(socket, Some(PathBuf::from("/r.sock")));
                assert_eq!(idle_timeout, Some(7));
            }
            _ => panic!("Expected Daemon Restart"),
        }
    }

    #[test]
    fn test_daemon_requires_subcommand() {
        // `daemon` with no subcommand is an error (it's a subcommand group).
        assert!(Cli::try_parse_from(["engramdb", "daemon"]).is_err());
    }

    #[test]
    fn test_stats_daemon_flag() {
        let cli = Cli::try_parse_from(["engramdb", "stats", "--daemon"]).unwrap();
        match cli.command {
            Command::Stats {
                daemon,
                global,
                all_projects,
            } => {
                assert!(daemon);
                assert!(!global);
                assert!(!all_projects);
            }
            _ => panic!("Expected Stats command"),
        }
        // Defaults: --daemon off.
        match Cli::try_parse_from(["engramdb", "stats"]).unwrap().command {
            Command::Stats { daemon, .. } => assert!(!daemon),
            _ => panic!("Expected Stats command"),
        }
    }

    // List command parsing (6 tests)
    #[test]
    fn test_list_multiple_type_filters() {
        let cli =
            Cli::try_parse_from(["engramdb", "list", "-t", "decision", "-t", "hazard"]).unwrap();
        match cli.command {
            Command::List { type_, .. } => {
                assert_eq!(type_, vec!["decision", "hazard"]);
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_list_tags_comma_delimiter() {
        let cli = Cli::try_parse_from(["engramdb", "list", "--tags", "a,b,c"]).unwrap();
        match cli.command {
            Command::List { tags, .. } => {
                assert_eq!(tags, vec!["a", "b", "c"]);
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_list_tags_repeated() {
        let cli = Cli::try_parse_from(["engramdb", "list", "--tags", "a", "--tags", "b"]).unwrap();
        match cli.command {
            Command::List { tags, .. } => {
                assert_eq!(tags, vec!["a", "b"]);
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_list_sort_values() {
        for sort_val in &["criticality", "created", "updated", "type"] {
            let cli = Cli::try_parse_from(["engramdb", "list", "--sort", sort_val]).unwrap();
            match cli.command {
                Command::List { sort, .. } => {
                    assert_eq!(sort, *sort_val);
                }
                _ => panic!("Expected List command"),
            }
        }
    }

    #[test]
    fn test_list_limit_parsing() {
        let cli = Cli::try_parse_from(["engramdb", "list", "--limit", "5"]).unwrap();
        match cli.command {
            Command::List { limit, .. } => {
                assert_eq!(limit, Some(5));
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_list_combined_sort_reverse_limit() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "list",
            "--sort",
            "created",
            "--reverse",
            "--limit",
            "3",
        ])
        .unwrap();
        match cli.command {
            Command::List {
                sort,
                reverse,
                limit,
                ..
            } => {
                assert_eq!(sort, "created");
                assert!(reverse);
                assert_eq!(limit, Some(3));
            }
            _ => panic!("Expected List command"),
        }
    }

    // Query (filter-mode) parsing — covers the old search surface.
    #[test]
    fn test_search_multiple_type_filters() {
        let cli = Cli::try_parse_from([
            "engramdb", "query", "--mode", "filter", "foo", "-t", "decision", "-t", "hazard",
        ])
        .unwrap();
        match cli.command {
            Command::Query { type_, .. } => {
                assert_eq!(type_, vec!["decision", "hazard"]);
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_search_physical_scope() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "query",
            "--mode",
            "filter",
            "foo",
            "-p",
            "src/main.rs",
        ])
        .unwrap();
        match cli.command {
            Command::Query { path, .. } => {
                assert_eq!(path, Some("src/main.rs".to_string()));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_search_multiple_logical_scopes() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "query",
            "--mode",
            "filter",
            "foo",
            "-l",
            "db.schema",
            "-l",
            "app.core",
        ])
        .unwrap();
        match cli.command {
            Command::Query { logical, .. } => {
                assert_eq!(logical, vec!["db.schema", "app.core"]);
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_search_min_criticality() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "query",
            "--mode",
            "filter",
            "foo",
            "--min-criticality",
            "0.5",
        ])
        .unwrap();
        match cli.command {
            Command::Query {
                min_criticality, ..
            } => {
                assert_eq!(min_criticality, Some(0.5));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_search_max_results() {
        let cli = Cli::try_parse_from(["engramdb", "query", "--mode", "filter", "foo", "-n", "5"])
            .unwrap();
        match cli.command {
            Command::Query { max_results, .. } => {
                assert_eq!(max_results, 5);
            }
            _ => panic!("Expected Query command"),
        }
    }

    // Query (rank-mode) parsing — covers the old retrieve surface.
    #[test]
    fn test_retrieve_multiple_type_filters() {
        let cli = Cli::try_parse_from([
            "engramdb", "query", "--mode", "rank", "--path", "x", "-t", "decision", "-t", "hazard",
        ])
        .unwrap();
        match cli.command {
            Command::Query { type_, .. } => {
                assert_eq!(type_, vec!["decision", "hazard"]);
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_retrieve_tags_filter() {
        let cli = Cli::try_parse_from([
            "engramdb", "query", "--mode", "rank", "--path", "x", "--tags", "a,b",
        ])
        .unwrap();
        match cli.command {
            Command::Query { tags, .. } => {
                assert_eq!(tags, vec!["a", "b"]);
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_retrieve_min_criticality() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "query",
            "--mode",
            "rank",
            "--path",
            "x",
            "--min-criticality",
            "0.5",
        ])
        .unwrap();
        match cli.command {
            Command::Query {
                min_criticality, ..
            } => {
                assert_eq!(min_criticality, Some(0.5));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_retrieve_include_expired() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "query",
            "--mode",
            "rank",
            "--path",
            "x",
            "--include-expired",
        ])
        .unwrap();
        match cli.command {
            Command::Query {
                include_expired, ..
            } => {
                assert!(include_expired);
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_retrieve_detail_levels() {
        for level in &["summary", "content", "full"] {
            let cli = Cli::try_parse_from([
                "engramdb",
                "query",
                "--mode",
                "rank",
                "--path",
                "x",
                "--detail-level",
                level,
            ])
            .unwrap();
            match cli.command {
                Command::Query { detail_level, .. } => {
                    assert_eq!(detail_level, Some(level.to_string()));
                }
                _ => panic!("Expected Query command"),
            }
        }
    }

    #[test]
    fn test_retrieve_multiple_logical_scopes() {
        let cli =
            Cli::try_parse_from(["engramdb", "query", "--mode", "rank", "-l", "a", "-l", "b"])
                .unwrap();
        match cli.command {
            Command::Query { logical, .. } => {
                assert_eq!(logical, vec!["a", "b"]);
            }
            _ => panic!("Expected Query command"),
        }
    }

    // Add command parsing (4 tests)
    #[test]
    fn test_add_multiple_physical_scopes() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "test",
            "-s",
            "test",
            "-p",
            "src/*.rs",
            "-p",
            "tests/*.rs",
        ])
        .unwrap();
        match cli.command {
            Command::Add { physical, .. } => {
                assert_eq!(physical, vec!["src/*.rs", "tests/*.rs"]);
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_multiple_logical_scopes() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "test",
            "-s",
            "test",
            "-l",
            "app.core",
            "-l",
            "db.schema",
        ])
        .unwrap();
        match cli.command {
            Command::Add { logical, .. } => {
                assert_eq!(logical, vec!["app.core", "db.schema"]);
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_confidence_default() {
        let cli = Cli::try_parse_from([
            "engramdb", "add", "-t", "decision", "-c", "test", "-s", "test",
        ])
        .unwrap();
        match cli.command {
            Command::Add { confidence, .. } => {
                assert!((confidence - 0.8).abs() < f64::EPSILON);
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_all_optional_flags() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--tags",
            "a,b",
            "-p",
            "src/main.rs",
            "-l",
            "app.core",
            "--criticality",
            "0.9",
            "--confidence",
            "0.7",
            "--details",
            "extra info",
            "--visibility",
            "personal",
            "--details-file",
            "/tmp/test.txt",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                type_,
                content,
                summary,
                tags,
                physical,
                logical,
                criticality,
                confidence,
                details,
                visibility,
                details_file,
                ..
            } => {
                assert_eq!(type_, Some("decision".to_string()));
                assert_eq!(content, Some("content".to_string()));
                assert_eq!(summary, Some("summary".to_string()));
                assert_eq!(tags, vec!["a", "b"]);
                assert_eq!(physical, vec!["src/main.rs"]);
                assert_eq!(logical, vec!["app.core"]);
                assert_eq!(criticality, Some(0.9));
                assert!((confidence - 0.7).abs() < f64::EPSILON);
                assert_eq!(details, Some("extra info".to_string()));
                assert_eq!(visibility, Some("personal".to_string()));
                assert_eq!(
                    details_file,
                    Some(std::path::PathBuf::from("/tmp/test.txt"))
                );
            }
            _ => panic!("Expected Add command"),
        }
    }

    // Update command parsing (4 tests)
    #[test]
    fn test_update_all_fields() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "update",
            "abc123",
            "-t",
            "convention",
            "-c",
            "new content",
            "-s",
            "new summary",
            "-p",
            "src/lib.rs",
            "-l",
            "app.core",
            "--tags",
            "x,y",
            "--criticality",
            "0.5",
            "--confidence",
            "0.6",
            "--details",
            "detail text",
            "--visibility",
            "personal",
            "--status",
            "needsreview",
        ])
        .unwrap();
        match cli.command {
            Command::Update {
                id,
                type_,
                content,
                summary,
                physical,
                logical,
                tags,
                criticality,
                confidence,
                details,
                visibility,
                status,
                ..
            } => {
                assert_eq!(id, "abc123");
                assert_eq!(type_, Some("convention".to_string()));
                assert_eq!(content, Some("new content".to_string()));
                assert_eq!(summary, Some("new summary".to_string()));
                assert_eq!(physical, vec!["src/lib.rs"]);
                assert_eq!(logical, vec!["app.core"]);
                assert_eq!(tags, vec!["x", "y"]);
                assert_eq!(criticality, Some(0.5));
                assert_eq!(confidence, Some(0.6));
                assert_eq!(details, Some("detail text".to_string()));
                assert_eq!(visibility, Some("personal".to_string()));
                assert_eq!(status, Some("needsreview".to_string()));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_tags_add_and_remove() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "update",
            "abc123",
            "--tags-add",
            "a",
            "--tags-remove",
            "b",
        ])
        .unwrap();
        match cli.command {
            Command::Update {
                tags_add,
                tags_remove,
                ..
            } => {
                assert_eq!(tags_add, Some("a".to_string()));
                assert_eq!(tags_remove, Some("b".to_string()));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_details_file() {
        let cli =
            Cli::try_parse_from(["engramdb", "update", "abc123", "--details-file", "path.txt"])
                .unwrap();
        match cli.command {
            Command::Update { details_file, .. } => {
                assert_eq!(details_file, Some(std::path::PathBuf::from("path.txt")));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_confidence() {
        let cli =
            Cli::try_parse_from(["engramdb", "update", "abc123", "--confidence", "0.9"]).unwrap();
        match cli.command {
            Command::Update { confidence, .. } => {
                assert_eq!(confidence, Some(0.9));
            }
            _ => panic!("Expected Update command"),
        }
    }

    // Global flags / conflicts (3 tests)
    #[test]
    fn test_json_flag_and_format_json_both_set() {
        // #18: `--json` and `--format` are now mutually exclusive, so supplying
        // both (even `--format json`) is rejected rather than silently accepted.
        assert!(
            Cli::try_parse_from(["engramdb", "--json", "--format", "json", "list"]).is_err(),
            "--json and --format must conflict even when both request JSON"
        );
    }

    #[test]
    fn test_verbose_and_quiet_both_parse() {
        let cli = Cli::try_parse_from(["engramdb", "-v", "-q", "list"]).unwrap();
        assert!(cli.verbose);
        assert!(cli.quiet);
    }

    #[test]
    fn test_embedding_backend_values() {
        for backend in &["onnx", "ollama", "auto"] {
            let result = Cli::try_parse_from(["engramdb", "--embedding-backend", backend, "list"]);
            assert!(
                result.is_ok(),
                "Failed to parse --embedding-backend {}: {:?}",
                backend,
                result.err()
            );
        }
    }

    // Miscellaneous commands (3 tests)
    #[test]
    fn test_compress_with_scope() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "compress",
            "--scope",
            "app.core",
            "--threshold",
            "0.3",
        ])
        .unwrap();
        match cli.command {
            Command::Compress {
                scope, threshold, ..
            } => {
                assert_eq!(scope, Some("app.core".to_string()));
                assert_eq!(threshold, Some(0.3));
            }
            _ => panic!("Expected Compress command"),
        }
    }

    #[test]
    fn test_review_all_flags() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "review",
            "--scope",
            "x",
            "--type",
            "decision",
            "--challenged-only",
            "--stale-only",
        ])
        .unwrap();
        match cli.command {
            Command::Review {
                scope,
                type_,
                challenged_only,
                stale_only,
                ..
            } => {
                assert_eq!(scope, Some("x".to_string()));
                assert_eq!(type_, Some("decision".to_string()));
                assert!(challenged_only);
                assert!(stale_only);
            }
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_review_stale_after_days_parsing() {
        // Flag omitted ⇒ None (review only flagged memories).
        let cli = Cli::try_parse_from(["engramdb", "review"]).unwrap();
        match cli.command {
            Command::Review {
                stale_after_days, ..
            } => assert_eq!(stale_after_days, None),
            _ => panic!("Expected Review command"),
        }

        // Bare flag ⇒ Some(90) (the default window).
        let cli = Cli::try_parse_from(["engramdb", "review", "--stale-after-days"]).unwrap();
        match cli.command {
            Command::Review {
                stale_after_days, ..
            } => assert_eq!(stale_after_days, Some(90)),
            _ => panic!("Expected Review command"),
        }

        // Explicit value ⇒ Some(N).
        let cli = Cli::try_parse_from(["engramdb", "review", "--stale-after-days", "30"]).unwrap();
        match cli.command {
            Command::Review {
                stale_after_days, ..
            } => assert_eq!(stale_after_days, Some(30)),
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_projects_delete_parsing() {
        let cli =
            Cli::try_parse_from(["engramdb", "projects", "delete", "some-id", "--force"]).unwrap();
        match cli.command {
            Command::Projects {
                command:
                    Some(ProjectsCommand::Delete {
                        project_id,
                        force,
                        cascade,
                        purge,
                    }),
            } => {
                assert_eq!(project_id, "some-id");
                assert!(force);
                assert!(!cascade);
                assert!(!purge, "personal memories are kept unless asked for");
            }
            _ => panic!("Expected Projects Delete command"),
        }
    }

    #[test]
    fn test_projects_delete_cascade_parsing() {
        let cli = Cli::try_parse_from(["engramdb", "projects", "delete", "some-id", "--cascade"])
            .unwrap();
        match cli.command {
            Command::Projects {
                command: Some(ProjectsCommand::Delete { cascade, .. }),
            } => assert!(cascade),
            _ => panic!("Expected Projects Delete command"),
        }
    }

    #[test]
    fn test_projects_link_parsing() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "projects",
            "link",
            "child-id",
            "--parent",
            "parent-id",
        ])
        .unwrap();
        match cli.command {
            Command::Projects {
                command: Some(ProjectsCommand::Link { child, parent }),
            } => {
                assert_eq!(child, "child-id");
                assert_eq!(parent, "parent-id");
            }
            _ => panic!("Expected Projects Link command"),
        }
    }

    #[test]
    fn test_global_flag_parses_for_single_store_commands() {
        let cli = Cli::try_parse_from(["engramdb", "list", "--global"]).unwrap();
        match cli.command {
            Command::List { global, .. } => assert!(global),
            _ => panic!("Expected List command"),
        }

        let cli = Cli::try_parse_from([
            "engramdb", "add", "-t", "decision", "-c", "x", "-s", "y", "--global",
        ])
        .unwrap();
        match cli.command {
            Command::Add { global, .. } => assert!(global),
            _ => panic!("Expected Add command"),
        }

        let cli = Cli::try_parse_from(["engramdb", "query", "--mode", "rank", "--global"]).unwrap();
        match cli.command {
            Command::Query { global, .. } => assert!(global),
            _ => panic!("Expected Query command"),
        }

        let cli = Cli::try_parse_from(["engramdb", "migrate", "--global"]).unwrap();
        match cli.command {
            Command::Migrate { global, .. } => assert!(global),
            _ => panic!("Expected Migrate command"),
        }
    }

    #[test]
    fn test_global_flag_defaults_false() {
        let cli = Cli::try_parse_from(["engramdb", "stats"]).unwrap();
        match cli.command {
            Command::Stats { global, .. } => assert!(!global),
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn test_query_include_global_flag_parses() {
        let cli = Cli::try_parse_from(["engramdb", "query", "--mode", "rank", "--include-global"])
            .unwrap();
        match cli.command {
            Command::Query {
                include_global,
                global,
                ..
            } => {
                assert!(include_global);
                assert!(!global, "--include-global must not imply --global");
            }
            _ => panic!("Expected Query command"),
        }

        // Defaults to false when omitted.
        let cli = Cli::try_parse_from(["engramdb", "query", "--mode", "rank"]).unwrap();
        match cli.command {
            Command::Query { include_global, .. } => assert!(!include_global),
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_setup_global_flag_remains_independent() {
        // `setup --global` predates the store-targeting flag and means
        // "install to ~/.claude/" — it must keep parsing on its own.
        let cli = Cli::try_parse_from(["engramdb", "setup", "--global"]).unwrap();
        match cli.command {
            Command::Setup { global, .. } => assert!(global),
            _ => panic!("Expected Setup command"),
        }
    }

    #[test]
    fn test_projects_unlink_parsing() {
        let cli = Cli::try_parse_from(["engramdb", "projects", "unlink", "child-id"]).unwrap();
        match cli.command {
            Command::Projects {
                command: Some(ProjectsCommand::Unlink { project_id }),
            } => assert_eq!(project_id, "child-id"),
            _ => panic!("Expected Projects Unlink command"),
        }
    }

    // ---- groups (multi-project memory membership) ----

    #[test]
    fn test_groups_create_parsing() {
        let cli = Cli::try_parse_from(["engramdb", "groups", "create", "Backend Family"]).unwrap();
        match cli.command {
            Command::Groups {
                command: GroupsCommand::Create { name },
            } => assert_eq!(name, "Backend Family"),
            _ => panic!("Expected Groups Create command"),
        }
    }

    #[test]
    fn test_groups_subscribe_unsubscribe_parsing() {
        // Default: no --yes flag.
        let cli = Cli::try_parse_from(["engramdb", "groups", "subscribe", "grp"]).unwrap();
        match cli.command {
            Command::Groups {
                command: GroupsCommand::Subscribe { name, yes },
            } => {
                assert_eq!(name, "grp");
                assert!(!yes);
            }
            _ => panic!("Expected Groups Subscribe command"),
        }

        // --yes bypasses the confirmation prompt.
        let cli =
            Cli::try_parse_from(["engramdb", "groups", "unsubscribe", "grp", "--yes"]).unwrap();
        match cli.command {
            Command::Groups {
                command: GroupsCommand::Unsubscribe { name, yes },
            } => {
                assert_eq!(name, "grp");
                assert!(yes);
            }
            _ => panic!("Expected Groups Unsubscribe command"),
        }
    }

    #[test]
    fn test_groups_list_parsing() {
        let cli = Cli::try_parse_from(["engramdb", "groups", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Groups {
                command: GroupsCommand::List
            }
        ));
    }

    #[test]
    fn test_groups_members_parsing() {
        let cli = Cli::try_parse_from(["engramdb", "groups", "members", "grp"]).unwrap();
        match cli.command {
            Command::Groups {
                command: GroupsCommand::Members { name },
            } => assert_eq!(name, "grp"),
            _ => panic!("Expected Groups Members command"),
        }
    }

    #[test]
    fn test_groups_requires_subcommand() {
        // Unlike `projects` (which defaults to Info), `groups` requires an
        // explicit subcommand.
        assert!(Cli::try_parse_from(["engramdb", "groups"]).is_err());
    }

    #[test]
    fn test_add_group_flag_parses_and_conflicts_with_global() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "x",
            "-s",
            "y",
            "--group",
            "Backend Family",
        ])
        .unwrap();
        match cli.command {
            Command::Add { group, global, .. } => {
                assert_eq!(group.as_deref(), Some("Backend Family"));
                assert!(!global);
            }
            _ => panic!("Expected Add command"),
        }

        // `--group` and `--global` are mutually exclusive.
        assert!(Cli::try_parse_from([
            "engramdb", "add", "-t", "decision", "-c", "x", "-s", "y", "--group", "g", "--global",
        ])
        .is_err());
    }

    // Add: supersedes and decay param parsing (7 tests)
    #[test]
    fn test_add_supersedes_flag() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--supersedes",
            "id1,id2,id3",
        ])
        .unwrap();
        match cli.command {
            Command::Add { supersedes, .. } => {
                assert_eq!(supersedes, Some("id1,id2,id3".to_string()));
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_audience_flag() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--group",
            "backend-family",
            "--audience",
            "projA,__g_x",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                audience, group, ..
            } => {
                assert_eq!(audience, vec!["projA".to_string(), "__g_x".to_string()]);
                assert_eq!(group, Some("backend-family".to_string()));
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_audience_defaults_empty() {
        let cli = Cli::try_parse_from([
            "engramdb", "add", "-t", "decision", "-c", "content", "-s", "summary",
        ])
        .unwrap();
        match cli.command {
            Command::Add { audience, .. } => assert!(audience.is_empty()),
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_decay_strategy() {
        for strategy in &["none", "linear", "exponential", "step"] {
            let cli = Cli::try_parse_from([
                "engramdb",
                "add",
                "-t",
                "decision",
                "-c",
                "content",
                "-s",
                "summary",
                "--decay-strategy",
                strategy,
            ])
            .unwrap();
            match cli.command {
                Command::Add { decay_strategy, .. } => {
                    assert_eq!(decay_strategy, Some(strategy.to_string()));
                }
                _ => panic!("Expected Add command"),
            }
        }
    }

    #[test]
    fn test_add_decay_half_life() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--decay-half-life",
            "3600",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                decay_half_life, ..
            } => {
                assert_eq!(decay_half_life, Some(3600));
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_decay_ttl() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--decay-ttl",
            "7200",
        ])
        .unwrap();
        match cli.command {
            Command::Add { decay_ttl, .. } => {
                assert_eq!(decay_ttl, Some(7200));
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_decay_floor() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--decay-floor",
            "0.1",
        ])
        .unwrap();
        match cli.command {
            Command::Add { decay_floor, .. } => {
                assert_eq!(decay_floor, Some(0.1));
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_all_decay_params_combined() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "add",
            "-t",
            "decision",
            "-c",
            "content",
            "-s",
            "summary",
            "--supersedes",
            "old-id",
            "--decay-strategy",
            "exponential",
            "--decay-half-life",
            "3600",
            "--decay-ttl",
            "86400",
            "--decay-floor",
            "0.05",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                supersedes,
                decay_strategy,
                decay_half_life,
                decay_ttl,
                decay_floor,
                ..
            } => {
                assert_eq!(supersedes, Some("old-id".to_string()));
                assert_eq!(decay_strategy, Some("exponential".to_string()));
                assert_eq!(decay_half_life, Some(3600));
                assert_eq!(decay_ttl, Some(86400));
                assert_eq!(decay_floor, Some(0.05));
            }
            _ => panic!("Expected Add command"),
        }
    }

    #[test]
    fn test_add_decay_defaults_to_none() {
        let cli = Cli::try_parse_from([
            "engramdb", "add", "-t", "decision", "-c", "content", "-s", "summary",
        ])
        .unwrap();
        match cli.command {
            Command::Add {
                supersedes,
                decay_strategy,
                decay_half_life,
                decay_ttl,
                decay_floor,
                ..
            } => {
                assert_eq!(supersedes, None);
                assert_eq!(decay_strategy, None);
                assert_eq!(decay_half_life, None);
                assert_eq!(decay_ttl, None);
                assert_eq!(decay_floor, None);
            }
            _ => panic!("Expected Add command"),
        }
    }

    // Update: decay param parsing (5 tests)
    #[test]
    fn test_update_decay_strategy() {
        let cli =
            Cli::try_parse_from(["engramdb", "update", "abc123", "--decay-strategy", "linear"])
                .unwrap();
        match cli.command {
            Command::Update { decay_strategy, .. } => {
                assert_eq!(decay_strategy, Some("linear".to_string()));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_decay_half_life() {
        let cli =
            Cli::try_parse_from(["engramdb", "update", "abc123", "--decay-half-life", "1800"])
                .unwrap();
        match cli.command {
            Command::Update {
                decay_half_life, ..
            } => {
                assert_eq!(decay_half_life, Some(1800));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_decay_ttl() {
        let cli =
            Cli::try_parse_from(["engramdb", "update", "abc123", "--decay-ttl", "3600"]).unwrap();
        match cli.command {
            Command::Update { decay_ttl, .. } => {
                assert_eq!(decay_ttl, Some(3600));
            }
            _ => panic!("Expected Update command"),
        }
    }

    #[test]
    fn test_update_decay_floor() {
        let cli =
            Cli::try_parse_from(["engramdb", "update", "abc123", "--decay-floor", "0.2"]).unwrap();
        match cli.command {
            Command::Update { decay_floor, .. } => {
                assert_eq!(decay_floor, Some(0.2));
            }
            _ => panic!("Expected Update command"),
        }
    }

    // Doctor command parsing
    #[test]
    fn test_doctor_no_subcommand() {
        let cli = Cli::try_parse_from(["engramdb", "doctor"]).unwrap();
        match cli.command {
            Command::Doctor { command, .. } => {
                assert!(command.is_none());
            }
            _ => panic!("Expected Doctor command"),
        }
    }

    #[test]
    fn test_doctor_store_subcommand() {
        let cli = Cli::try_parse_from(["engramdb", "doctor", "store"]).unwrap();
        match cli.command {
            Command::Doctor {
                command: Some(DoctorCommand::Store),
                ..
            } => {} // expected
            _ => panic!("Expected Doctor Store subcommand"),
        }
    }

    #[test]
    fn test_update_all_decay_params_combined() {
        let cli = Cli::try_parse_from([
            "engramdb",
            "update",
            "abc123",
            "--decay-strategy",
            "step",
            "--decay-half-life",
            "7200",
            "--decay-ttl",
            "14400",
            "--decay-floor",
            "0.15",
        ])
        .unwrap();
        match cli.command {
            Command::Update {
                decay_strategy,
                decay_half_life,
                decay_ttl,
                decay_floor,
                ..
            } => {
                assert_eq!(decay_strategy, Some("step".to_string()));
                assert_eq!(decay_half_life, Some(7200));
                assert_eq!(decay_ttl, Some(14400));
                assert_eq!(decay_floor, Some(0.15));
            }
            _ => panic!("Expected Update command"),
        }
    }

    /// `harvest mark --memory ""` used to be accepted: the ledger then
    /// recorded `memory_ids: [""]` with `memories_created: 1`, so
    /// `ledger show` claimed a memory and printed a blank line where its id
    /// should have been. Rejecting at parse names the flag and writes nothing.
    #[test]
    fn harvest_mark_rejects_an_empty_memory_id() {
        for blank in ["", "   ", "\t"] {
            let parsed =
                Cli::try_parse_from(["engramdb", "harvest", "mark", "s1", "--memory", blank]);
            let Err(err) = parsed else {
                panic!("an empty --memory must not parse (blank {blank:?})");
            };
            let rendered = err.to_string();
            assert!(
                rendered.contains("cannot be empty"),
                "the error must name the problem, got: {rendered}"
            );
        }

        // The zero-yield form the message points at still works, and a real
        // id is untouched (trimmed, not rejected).
        let cli = Cli::try_parse_from(["engramdb", "harvest", "mark", "s1"]).unwrap();
        match cli.command {
            Command::Harvest {
                command: HarvestCommand::Mark { memory_ids, .. },
            } => assert!(memory_ids.is_empty()),
            _ => panic!("expected harvest mark"),
        }
        let cli =
            Cli::try_parse_from(["engramdb", "harvest", "mark", "s1", "--memory", " abc123 "])
                .unwrap();
        match cli.command {
            Command::Harvest {
                command: HarvestCommand::Mark { memory_ids, .. },
            } => assert_eq!(memory_ids, vec!["abc123".to_string()]),
            _ => panic!("expected harvest mark"),
        }
    }

    /// `--stage` enumerated its values while `--decision` did not, so the one
    /// flag whose vocabulary is not guessable was the one `--help` withheld.
    #[test]
    fn ledger_list_help_enumerates_both_filters() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd
            .find_subcommand_mut("harvest")
            .and_then(|c| c.find_subcommand_mut("ledger"))
            .and_then(|c| c.find_subcommand_mut("list"))
            .expect("harvest ledger list exists")
            .render_long_help()
            .to_string();
        for value in ["harvested", "skipped", "deferred", "unreviewed"] {
            assert!(help.contains(value), "--decision must list {value}: {help}");
        }
        for value in ["collected", "indexed", "compressed"] {
            assert!(help.contains(value), "--stage must list {value}: {help}");
        }
    }

    /// `harvest --help` named only `list`/`show`/`mark`, omitting the whole
    /// search-and-ledger surface a reader would otherwise never learn about
    /// from the long description.
    #[test]
    fn harvest_long_help_names_every_subcommand() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let harvest = cmd
            .find_subcommand_mut("harvest")
            .expect("harvest exists")
            .clone();
        let help = harvest.clone().render_long_help().to_string();
        for name in harvest
            .get_subcommands()
            .map(|s| s.get_name().to_string())
            .filter(|n| n != "help")
        {
            assert!(
                help.contains(&format!("`{name}`")),
                "the long description must name `{name}`: {help}"
            );
        }
    }
}
