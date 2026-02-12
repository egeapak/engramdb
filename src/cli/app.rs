//! Clap CLI application definitions.
//!
//! This module defines the command-line interface structure using Clap's derive macros.
//! It includes the main CLI struct, all subcommands, and output format options.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Subcommands for `engramdb projects`.
#[derive(Subcommand)]
pub enum ProjectsCommand {
    /// Show info about the current project (default)
    Info,
    /// List all registered projects
    List,
    /// Remove a project from the registry and delete its global data
    Delete {
        /// Project ID to delete
        project_id: String,
        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,
    },
    /// Show aggregate statistics across all projects
    Stats,
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

        /// Launch interactive TUI prompts
        #[arg(long, short = 'i')]
        interactive: bool,

        /// Open $EDITOR for content entry
        #[arg(long, short = 'e')]
        editor: bool,

        /// Read details from file
        #[arg(long)]
        details_file: Option<PathBuf>,
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
    },

    /// Retrieve memories by context
    Retrieve {
        /// Physical path context
        #[arg(long)]
        path: Option<String>,

        /// Logical scope context (can be repeated)
        #[arg(long, short = 'l')]
        logical: Vec<String>,

        /// Query text for semantic search
        #[arg(long)]
        query: Option<String>,

        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by tags (comma-separated or repeated)
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Minimum criticality
        #[arg(long)]
        min_criticality: Option<f64>,

        /// Maximum number of results
        #[arg(long, short = 'n', default_value = "10")]
        max_results: usize,

        /// Detail level: summary, content, full
        #[arg(long)]
        detail_level: Option<String>,

        /// Include expired memories
        #[arg(long)]
        include_expired: bool,

        /// Show relevance scores alongside results
        #[arg(long)]
        show_scores: bool,
    },

    /// Search memories by keyword
    Search {
        /// Search query
        query: String,

        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by tags (comma-separated or repeated)
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,

        /// Filter by physical scope
        #[arg(long, short = 'p')]
        physical: Option<String>,

        /// Filter by logical scope (can be repeated)
        #[arg(long, short = 'l')]
        logical: Vec<String>,

        /// Minimum criticality
        #[arg(long)]
        min_criticality: Option<f64>,

        /// Maximum number of results to display
        #[arg(long, short = 'n', default_value = "10")]
        max_results: usize,
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

        /// Open memory file in $EDITOR
        #[arg(long, short = 'e')]
        editor: bool,
    },

    /// Delete a memory
    Delete {
        /// Memory ID (supports prefix matching)
        id: String,

        /// Skip confirmation prompt
        #[arg(long, short = 'f')]
        force: bool,
    },

    /// Show statistics
    Stats,

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
    },

    /// Run garbage collection on low-relevance memories
    Gc {
        /// Actually delete (default is dry-run)
        #[arg(long)]
        confirm: bool,

        /// Score threshold for GC (default from config)
        #[arg(long)]
        threshold: Option<f64>,
    },

    /// List compression candidates (actual compression requires MCP mode)
    Compress {
        /// Filter by logical scope
        #[arg(long)]
        scope: Option<String>,

        /// Criticality threshold for candidates (default 0.4)
        #[arg(long)]
        threshold: Option<f64>,
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

    /// Generate shell completions
    Completions {
        /// Shell type
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Rebuild index and re-embed memories
    Reindex {
        /// Only re-embed, don't rebuild index
        #[arg(long)]
        embeddings_only: bool,

        /// Only rebuild index, don't re-embed
        #[arg(long)]
        index_only: bool,
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
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retrieve_with_query_long_flag() {
        // Test that `retrieve --query test` works without conflicting with global `-q`
        let result = Cli::try_parse_from(["engramdb", "retrieve", "--query", "test"]);
        assert!(
            result.is_ok(),
            "Failed to parse retrieve --query: {:?}",
            result.err()
        );

        let cli = result.unwrap();
        match cli.command {
            Command::Retrieve { query, .. } => {
                assert_eq!(query, Some("test".to_string()));
            }
            _ => panic!("Expected Retrieve command"),
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
        let cli = Cli::try_parse_from(["engramdb", "search", "test query"]).unwrap();
        match cli.command {
            Command::Search { query, .. } => {
                assert_eq!(query, "test query");
            }
            _ => panic!("Expected Search command"),
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
            Command::Gc { confirm, threshold } => {
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
}
