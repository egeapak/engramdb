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
}

/// Subcommands for `engramdb doctor`.
#[derive(Subcommand)]
pub enum DoctorCommand {
    /// Fast store health check (index consistency only)
    Store,
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
    Status,
    /// Ask a running daemon to exit gracefully.
    Stop,
    /// Stop a running daemon (if any) and start a fresh one.
    Restart {
        /// Idle timeout for the newly started daemon.
        #[arg(long)]
        idle_timeout: Option<u64>,
    },
}

/// Subcommands for `engramdb projects`.
#[derive(Subcommand)]
pub enum ProjectsCommand {
    /// Show info about the current project (default)
    Info,
    /// List all registered projects
    List,
    /// Remove a project from the registry and delete its global data.
    ///
    /// Refuses by default when the project has sub-projects (children).
    /// Re-run with `--cascade` to also delete descendants, or unlink the
    /// children first with `engramdb projects unlink <id>`.
    Delete {
        /// Project ID to delete
        project_id: String,
        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,
        /// Also delete all descendants (children and their children).
        #[arg(long)]
        cascade: bool,
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
    #[arg(long, global = true, value_enum)]
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
    pub embedding_backend: Option<crate::types::EmbeddingBackend>,
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

        /// Brief summary (auto-generated if not provided)
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

        /// IDs of memories this one supersedes (comma-separated)
        #[arg(long)]
        supersedes: Option<String>,

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

        /// Show relevance scores alongside results.
        #[arg(long)]
        show_scores: bool,

        /// Also merge global (cross-project) memories into the results.
        ///
        /// Runs the same query against the global store and folds its hits
        /// into the project results (deduplicated, re-sorted, truncated).
        /// Ignored when `--global` is set (already querying the global store).
        #[arg(long)]
        include_global: bool,

        /// Query the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// List all memories
    List {
        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

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
        #[arg(long, value_delimiter = ',')]
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
    },

    /// Manage registered EngramDB projects
    Projects {
        #[command(subcommand)]
        command: Option<ProjectsCommand>,
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
        #[arg(long)]
        embeddings_only: bool,

        /// Only rebuild index, don't re-embed
        #[arg(long)]
        index_only: bool,

        /// Reindex the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },

    /// Claude Code plugin hook handler
    Hook {
        #[command(subcommand)]
        command: HookCommand,
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

        /// Review the global (cross-project) memory store instead of the current project
        #[arg(long)]
        global: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let cli = Cli::try_parse_from(["engramdb", "--json", "--format", "json", "list"]).unwrap();
        assert!(cli.json);
        match cli.format {
            Some(OutputFormat::Json) => {}
            other => panic!("Expected Json format, got {:?}", other),
        }
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
                    }),
            } => {
                assert_eq!(project_id, "some-id");
                assert!(force);
                assert!(!cascade);
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
}
