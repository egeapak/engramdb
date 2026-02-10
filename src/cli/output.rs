//! Output formatting for CLI commands.
//!
//! This module provides a unified output formatter that supports multiple output modes:
//! - **Pretty**: Human-friendly output with colors and formatting (for terminals).
//! - **JSON**: Structured JSON output for programmatic parsing.
//! - **Plain**: Simple text output without colors (for non-TTY environments).
//!
//! The formatter automatically detects terminal capabilities and adjusts formatting
//! accordingly.

use crate::storage::index::IndexEntry;
use crate::types::{Memory, MemoryType, Status};
use owo_colors::{OwoColorize, Stream};
use serde_json;
use std::io::{self, IsTerminal};

use super::app::OutputFormat;

/// Output formatter for CLI results.
///
/// Handles formatting and display of command results in different output modes.
/// Automatically detects terminal capabilities and adjusts formatting.
pub struct OutputFormatter {
    format: OutputFormat,
    use_color: bool,
}

impl OutputFormatter {
    /// Create a new output formatter.
    ///
    /// Automatically detects terminal capabilities and selects appropriate formatting.
    ///
    /// # Arguments
    /// * `format` - Explicit format selection (overrides defaults)
    /// * `json` - Force JSON output
    /// * `no_color` - Disable colored output
    pub fn new(format: Option<OutputFormat>, json: bool, no_color: bool) -> Self {
        let is_tty = io::stdout().is_terminal();

        let format = if json {
            OutputFormat::Json
        } else if let Some(fmt) = format {
            fmt
        } else if is_tty {
            OutputFormat::Pretty
        } else {
            OutputFormat::Json
        };

        let use_color = is_tty && !no_color && !matches!(format, OutputFormat::Json);

        Self { format, use_color }
    }

    /// Print a generic message.
    pub fn print_message(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::json!({ "message": message }));
            }
            OutputFormat::Pretty | OutputFormat::Plain => {
                println!("{}", message);
            }
        }
    }

    /// Print a success message (with green color in pretty mode).
    pub fn print_success(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({ "success": true, "message": message })
                );
            }
            OutputFormat::Pretty => {
                if self.use_color {
                    println!(
                        "{} {}",
                        "✓".if_supports_color(Stream::Stdout, |text| text.green()),
                        message.if_supports_color(Stream::Stdout, |text| text.green())
                    );
                } else {
                    println!("✓ {}", message);
                }
            }
            OutputFormat::Plain => {
                println!("{}", message);
            }
        }
    }

    /// Print an error message (with red color in pretty mode).
    pub fn print_error(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                eprintln!("{}", serde_json::json!({ "error": message }));
            }
            OutputFormat::Pretty => {
                if self.use_color {
                    eprintln!(
                        "{} {}",
                        "✗".if_supports_color(Stream::Stderr, |text| text.red()),
                        message.if_supports_color(Stream::Stderr, |text| text.red())
                    );
                } else {
                    eprintln!("✗ {}", message);
                }
            }
            OutputFormat::Plain => {
                eprintln!("Error: {}", message);
            }
        }
    }

    /// Print a single memory in the configured format.
    pub fn print_memory(&self, memory: &Memory) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(memory).unwrap());
            }
            OutputFormat::Pretty => {
                self.print_memory_pretty(memory);
            }
            OutputFormat::Plain => {
                self.print_memory_plain(memory);
            }
        }
    }

    /// Print a memory with full details without truncation.
    pub fn print_memory_full(&self, memory: &Memory) {
        // For now, this is identical to print_memory
        // In the future, print_memory might add truncation logic
        self.print_memory(memory);
    }

    fn print_memory_pretty(&self, memory: &Memory) {
        let id_display = if self.use_color {
            memory
                .id
                .if_supports_color(Stream::Stdout, |text| text.cyan())
                .to_string()
        } else {
            memory.id.clone()
        };

        let type_display = if self.use_color {
            format!("{:?}", memory.type_)
                .if_supports_color(Stream::Stdout, |text| text.yellow())
                .to_string()
        } else {
            format!("{:?}", memory.type_)
        };

        println!("ID: {}", id_display);
        println!("Type: {}", type_display);
        println!("Summary: {}", memory.summary);
        println!("Content: {}", memory.content);

        if let Some(ref details) = memory.details {
            println!("Details: {}", details);
        }

        if !memory.physical.is_empty() {
            println!("Physical: {}", memory.physical.join(", "));
        }

        if !memory.logical.is_empty() {
            println!("Logical: {}", memory.logical.join(", "));
        }

        if !memory.tags.is_empty() {
            println!("Tags: {}", memory.tags.join(", "));
        }

        println!("Criticality: {:.2}", memory.criticality);
        println!("Confidence: {:.2}", memory.confidence);
        println!("Status: {:?}", memory.status);
        println!("Visibility: {:?}", memory.visibility);
        println!("Created: {}", memory.created_at.format("%Y-%m-%d %H:%M:%S"));
        println!("Updated: {}", memory.updated_at.format("%Y-%m-%d %H:%M:%S"));
    }

    fn print_memory_plain(&self, memory: &Memory) {
        println!("ID: {}", memory.id);
        println!("Type: {:?}", memory.type_);
        println!("Summary: {}", memory.summary);
        println!("Content: {}", memory.content);

        if let Some(ref details) = memory.details {
            println!("Details: {}", details);
        }

        if !memory.physical.is_empty() {
            println!("Physical: {}", memory.physical.join(", "));
        }

        if !memory.logical.is_empty() {
            println!("Logical: {}", memory.logical.join(", "));
        }

        if !memory.tags.is_empty() {
            println!("Tags: {}", memory.tags.join(", "));
        }

        println!("Criticality: {:.2}", memory.criticality);
        println!("Confidence: {:.2}", memory.confidence);
        println!("Status: {:?}", memory.status);
        println!("Visibility: {:?}", memory.visibility);
    }

    /// Print a list of memory index entries in the configured format.
    pub fn print_memory_list(&self, entries: &[IndexEntry]) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(entries).unwrap());
            }
            OutputFormat::Pretty => {
                self.print_list_pretty(entries);
            }
            OutputFormat::Plain => {
                self.print_list_plain(entries);
            }
        }
    }

    fn print_list_pretty(&self, entries: &[IndexEntry]) {
        if entries.is_empty() {
            println!("No memories found.");
            return;
        }

        for entry in entries {
            let id_short = &entry.id[..8.min(entry.id.len())];
            let id_display = if self.use_color {
                id_short
                    .if_supports_color(Stream::Stdout, |text| text.cyan())
                    .to_string()
            } else {
                id_short.to_string()
            };

            let type_display = if self.use_color {
                format!("{:?}", entry.type_)
                    .if_supports_color(Stream::Stdout, |text| text.yellow())
                    .to_string()
            } else {
                format!("{:?}", entry.type_)
            };

            println!("{} {} {}", id_display, type_display, entry.summary);
        }
    }

    fn print_list_plain(&self, entries: &[IndexEntry]) {
        if entries.is_empty() {
            println!("No memories found.");
            return;
        }

        for entry in entries {
            let id_short = &entry.id[..8.min(entry.id.len())];
            println!("{} {:?} {}", id_short, entry.type_, entry.summary);
        }
    }

    /// Print statistics in the configured format.
    pub fn print_stats(&self, stats: &Stats) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(stats).unwrap());
            }
            OutputFormat::Pretty => {
                self.print_stats_pretty(stats);
            }
            OutputFormat::Plain => {
                self.print_stats_plain(stats);
            }
        }
    }

    fn print_stats_pretty(&self, stats: &Stats) {
        println!("Total Memories: {}", stats.total);
        println!("\nBy Type:");
        for (type_, count) in &stats.by_type {
            println!("  {:?}: {}", type_, count);
        }
        println!("\nBy Status:");
        for (status, count) in &stats.by_status {
            println!("  {:?}: {}", status, count);
        }
        if !stats.by_scope.is_empty() {
            println!("\nBy Scope:");
            for (scope, count) in &stats.by_scope {
                println!("  {}: {}", scope, count);
            }
        }
        println!("\nExpired: {}", stats.expired);
        if let Some(oldest) = stats.oldest {
            println!("Oldest: {}", oldest.format("%Y-%m-%d"));
        }
        if let Some(newest) = stats.newest {
            println!("Newest: {}", newest.format("%Y-%m-%d"));
        }
        println!("\nAverage Criticality: {:.2}", stats.avg_criticality);
    }

    fn print_stats_plain(&self, stats: &Stats) {
        println!("Total: {}", stats.total);
        for (type_, count) in &stats.by_type {
            println!("{:?}: {}", type_, count);
        }
    }
}

/// Statistics about the memory store.
#[derive(Debug, serde::Serialize)]
pub struct Stats {
    /// Total number of memories
    pub total: usize,
    /// Count of memories by type
    pub by_type: Vec<(MemoryType, usize)>,
    /// Count of memories by status
    pub by_status: Vec<(Status, usize)>,
    /// Count of memories per logical scope
    pub by_scope: Vec<(String, usize)>,
    /// Count of expired memories
    pub expired: usize,
    /// Oldest created_at timestamp
    pub oldest: Option<chrono::DateTime<chrono::Utc>>,
    /// Newest created_at timestamp
    pub newest: Option<chrono::DateTime<chrono::Utc>>,
    /// Average criticality across all memories
    pub avg_criticality: f64,
}
