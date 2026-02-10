use crate::types::{Memory, MemoryType, Status};
use crate::storage::index::IndexEntry;
use owo_colors::{OwoColorize, Stream};
use serde_json;
use std::io::{self, IsTerminal};

use super::app::OutputFormat;

pub struct OutputFormatter {
    format: OutputFormat,
    use_color: bool,
}

impl OutputFormatter {
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

    pub fn print_success(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::json!({ "success": true, "message": message }));
            }
            OutputFormat::Pretty => {
                if self.use_color {
                    println!("{} {}", "✓".if_supports_color(Stream::Stdout, |text| text.green()),
                             message.if_supports_color(Stream::Stdout, |text| text.green()));
                } else {
                    println!("✓ {}", message);
                }
            }
            OutputFormat::Plain => {
                println!("{}", message);
            }
        }
    }

    pub fn print_error(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                eprintln!("{}", serde_json::json!({ "error": message }));
            }
            OutputFormat::Pretty => {
                if self.use_color {
                    eprintln!("{} {}", "✗".if_supports_color(Stream::Stderr, |text| text.red()),
                              message.if_supports_color(Stream::Stderr, |text| text.red()));
                } else {
                    eprintln!("✗ {}", message);
                }
            }
            OutputFormat::Plain => {
                eprintln!("Error: {}", message);
            }
        }
    }

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

    fn print_memory_pretty(&self, memory: &Memory) {
        let id_display = if self.use_color {
            memory.id.if_supports_color(Stream::Stdout, |text| text.cyan()).to_string()
        } else {
            memory.id.clone()
        };

        let type_display = if self.use_color {
            format!("{:?}", memory.type_).if_supports_color(Stream::Stdout, |text| text.yellow()).to_string()
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
                id_short.if_supports_color(Stream::Stdout, |text| text.cyan()).to_string()
            } else {
                id_short.to_string()
            };

            let type_display = if self.use_color {
                format!("{:?}", entry.type_).if_supports_color(Stream::Stdout, |text| text.yellow()).to_string()
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
        if !stats.logical_scopes.is_empty() {
            println!("\nLogical Scopes:");
            for scope in &stats.logical_scopes {
                println!("  {}", scope);
            }
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

#[derive(Debug, serde::Serialize)]
pub struct Stats {
    pub total: usize,
    pub by_type: Vec<(MemoryType, usize)>,
    pub by_status: Vec<(Status, usize)>,
    pub logical_scopes: Vec<String>,
    pub avg_criticality: f64,
}
