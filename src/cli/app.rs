//! Clap CLI application definitions.
//!
//! This module defines the command-line interface structure using Clap's derive macros.
//! It includes the main CLI struct, all subcommands, and output format options.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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
    #[arg(long, global = true)]
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
}

/// Available CLI commands.
#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new EngramDB store
    Init,

    /// Add a new memory
    Add {
        /// Type of memory
        #[arg(long, short = 't', value_name = "TYPE")]
        type_: String,

        /// Memory content
        #[arg(long, short = 'c')]
        content: String,

        /// Brief summary (auto-generated if not provided)
        #[arg(long, short = 's')]
        summary: Option<String>,

        /// Physical scope (file paths or globs, can be repeated)
        #[arg(long, short = 'p')]
        physical: Vec<String>,

        /// Logical scope (dot-notation domains, can be repeated)
        #[arg(long, short = 'l')]
        logical: Vec<String>,

        /// Tags (can be repeated)
        #[arg(long)]
        tags: Vec<String>,

        /// Criticality score (0.0 to 1.0)
        #[arg(long, default_value = "0.5")]
        criticality: f64,

        /// Confidence score (0.0 to 1.0)
        #[arg(long, default_value = "0.8")]
        confidence: f64,

        /// Extended details
        #[arg(long)]
        details: Option<String>,

        /// Visibility (shared or personal)
        #[arg(long, default_value = "shared")]
        visibility: String,
    },

    /// Get a memory by ID
    Get {
        /// Memory ID (supports prefix matching)
        id: String,
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
        #[arg(long, short = 'q')]
        query: Option<String>,

        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by tags (can be repeated)
        #[arg(long)]
        tags: Vec<String>,

        /// Minimum criticality
        #[arg(long)]
        min_criticality: Option<f64>,

        /// Maximum number of results
        #[arg(long, default_value = "10")]
        max_results: usize,
    },

    /// Search memories by keyword
    Search {
        /// Search query
        query: String,

        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by tags (can be repeated)
        #[arg(long)]
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
    },

    /// List all memories
    List {
        /// Filter by type (can be repeated)
        #[arg(long, short = 't')]
        type_: Vec<String>,

        /// Filter by tags (can be repeated)
        #[arg(long)]
        tags: Vec<String>,

        /// Filter by status
        #[arg(long, short = 's')]
        status: Option<String>,
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

        /// New tags (replaces existing)
        #[arg(long)]
        tags: Vec<String>,

        /// New criticality
        #[arg(long)]
        criticality: Option<f64>,

        /// New confidence
        #[arg(long)]
        confidence: Option<f64>,

        /// New details
        #[arg(long)]
        details: Option<String>,

        /// New visibility
        #[arg(long)]
        visibility: Option<String>,

        /// New status
        #[arg(long)]
        status: Option<String>,
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

    /// Compress memories by scope
    Compress {
        /// Filter by logical scope
        #[arg(long)]
        scope: Option<String>,

        /// Score threshold for compression
        #[arg(long)]
        threshold: Option<f64>,

        /// List compression candidates
        #[arg(long)]
        confirm: bool,
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
    },
}
