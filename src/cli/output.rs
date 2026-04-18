//! Output formatting for CLI commands.
//!
//! This module provides a unified output formatter that supports multiple output modes:
//! - **Pretty**: Human-friendly output with colors and formatting (for terminals).
//! - **JSON**: Structured JSON output for programmatic parsing.
//! - **Plain**: Simple text output without colors (for non-TTY environments).
//!
//! The formatter automatically detects terminal capabilities and adjusts formatting
//! accordingly.

use crate::retrieval::engine::{RetrievalResult, ScoredMemory};
use crate::storage::IndexFilterable;
use crate::types::{Memory, MemoryType, Status};
use owo_colors::{OwoColorize, Stream};
use serde_json;
use std::io::{self, IsTerminal};

use super::app::OutputFormat;

/// Helper function to truncate IDs to 13 characters
pub fn short_id(id: &str) -> &str {
    &id[..13.min(id.len())]
}

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

    /// Print a hint/suggestion message (with blue color in pretty mode).
    pub fn print_hint(&self, message: &str) {
        match self.format {
            OutputFormat::Pretty => {
                if self.use_color {
                    println!(
                        "  {} {}",
                        "ℹ".if_supports_color(Stream::Stdout, |text| text.blue()),
                        message.if_supports_color(Stream::Stdout, |text| text.blue())
                    );
                } else {
                    println!("  ℹ {}", message);
                }
            }
            OutputFormat::Plain => {
                println!("  Hint: {}", message);
            }
            OutputFormat::Json => {} // hints are embedded in structured output
        }
    }

    /// Print full environment doctor results organized by section.
    pub fn print_environment_doctor(&self, result: &crate::ops::EnvironmentDoctorResult) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(result).unwrap());
            }
            OutputFormat::Pretty | OutputFormat::Plain => {
                let header = "EngramDB Environment Check";
                if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                    println!(
                        "\n{}",
                        header.if_supports_color(Stream::Stdout, |text| text.bold())
                    );
                } else {
                    println!("\n{}", header);
                }

                for section in &result.sections {
                    println!();
                    if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                        println!(
                            "{}",
                            section
                                .name
                                .if_supports_color(Stream::Stdout, |text| text.bold())
                        );
                    } else {
                        println!("{}", section.name);
                    }

                    for check in &section.checks {
                        use crate::ops::CheckStatus;

                        let (icon, style) = match check.status {
                            Some(CheckStatus::Info) => ("○", "info"),
                            Some(CheckStatus::Warn) => ("⚠", "warn"),
                            Some(CheckStatus::Pass) => ("✓", "pass"),
                            Some(CheckStatus::Fail) => ("✗", "fail"),
                            None if check.passed => ("✓", "pass"),
                            None => ("✗", "fail"),
                        };

                        if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                            let colored_icon = match style {
                                "info" => icon
                                    .if_supports_color(Stream::Stdout, |t| t.dimmed())
                                    .to_string(),
                                "warn" => icon
                                    .if_supports_color(Stream::Stdout, |t| t.yellow())
                                    .to_string(),
                                "pass" => icon
                                    .if_supports_color(Stream::Stdout, |t| t.green())
                                    .to_string(),
                                _ => icon
                                    .if_supports_color(Stream::Stdout, |t| t.red())
                                    .to_string(),
                            };
                            if style == "info" {
                                println!(
                                    "  {} {}: {}",
                                    colored_icon,
                                    check.name.if_supports_color(Stream::Stdout, |t| t.dimmed()),
                                    check
                                        .message
                                        .if_supports_color(Stream::Stdout, |t| t.dimmed()),
                                );
                            } else if style == "warn" {
                                println!(
                                    "  {} {}: {}",
                                    colored_icon,
                                    check.name.if_supports_color(Stream::Stdout, |t| t.yellow()),
                                    check.message,
                                );
                            } else {
                                println!("  {} {}: {}", colored_icon, check.name, check.message);
                            }
                        } else {
                            println!("  {} {}: {}", icon, check.name, check.message);
                        }
                        for detail in &check.details {
                            if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                                println!(
                                    "      {}",
                                    detail.if_supports_color(Stream::Stdout, |text| text.dimmed())
                                );
                            } else {
                                println!("      {}", detail);
                            }
                        }
                        if let Some(ref suggestion) = check.suggestion {
                            self.print_hint(suggestion);
                        }
                    }

                    for subsection in &section.subsections {
                        if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                            println!(
                                "  {}",
                                subsection
                                    .name
                                    .if_supports_color(Stream::Stdout, |text| text.dimmed())
                            );
                        } else {
                            println!("  {}", subsection.name);
                        }
                        for check in &subsection.checks {
                            use crate::ops::CheckStatus;

                            let (icon, style) = match check.status {
                                Some(CheckStatus::Info) => ("○", "info"),
                                Some(CheckStatus::Warn) => ("⚠", "warn"),
                                Some(CheckStatus::Pass) => ("✓", "pass"),
                                Some(CheckStatus::Fail) => ("✗", "fail"),
                                None if check.passed => ("✓", "pass"),
                                None => ("✗", "fail"),
                            };

                            if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                                let colored_icon = match style {
                                    "info" => icon
                                        .if_supports_color(Stream::Stdout, |t| t.dimmed())
                                        .to_string(),
                                    "warn" => icon
                                        .if_supports_color(Stream::Stdout, |t| t.yellow())
                                        .to_string(),
                                    "pass" => icon
                                        .if_supports_color(Stream::Stdout, |t| t.green())
                                        .to_string(),
                                    _ => icon
                                        .if_supports_color(Stream::Stdout, |t| t.red())
                                        .to_string(),
                                };
                                if style == "info" {
                                    println!(
                                        "    {} {}: {}",
                                        colored_icon,
                                        check
                                            .name
                                            .if_supports_color(Stream::Stdout, |t| t.dimmed()),
                                        check
                                            .message
                                            .if_supports_color(Stream::Stdout, |t| t.dimmed()),
                                    );
                                } else if style == "warn" {
                                    println!(
                                        "    {} {}: {}",
                                        colored_icon,
                                        check
                                            .name
                                            .if_supports_color(Stream::Stdout, |t| t.yellow()),
                                        check.message,
                                    );
                                } else {
                                    println!(
                                        "    {} {}: {}",
                                        colored_icon, check.name, check.message
                                    );
                                }
                            } else {
                                println!("    {} {}: {}", icon, check.name, check.message);
                            }
                            for detail in &check.details {
                                if self.use_color && matches!(self.format, OutputFormat::Pretty) {
                                    println!(
                                        "        {}",
                                        detail.if_supports_color(Stream::Stdout, |text| {
                                            text.dimmed()
                                        })
                                    );
                                } else {
                                    println!("        {}", detail);
                                }
                            }
                            if let Some(ref suggestion) = check.suggestion {
                                self.print_hint(suggestion);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Print a warning message (with yellow color in pretty mode).
    pub fn print_warning(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                eprintln!("{}", serde_json::json!({ "warning": message }));
            }
            OutputFormat::Pretty => {
                if self.use_color {
                    eprintln!(
                        "{} {}",
                        "⚠".if_supports_color(Stream::Stderr, |text| text.yellow()),
                        message.if_supports_color(Stream::Stderr, |text| text.yellow())
                    );
                } else {
                    eprintln!("Warning: {}", message);
                }
            }
            OutputFormat::Plain => {
                eprintln!("Warning: {}", message);
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

    /// Print search results in the configured format.
    pub fn print_search_results(&self, results: &[ScoredMemory]) {
        match self.format {
            OutputFormat::Json => {
                let json_output = results
                    .iter()
                    .map(|sm| {
                        serde_json::json!({
                            "memory": sm.memory,
                            "score": sm.score,
                        })
                    })
                    .collect::<Vec<_>>();
                println!("{}", serde_json::to_string_pretty(&json_output).unwrap());
            }
            OutputFormat::Pretty => {
                self.print_search_results_pretty(results);
            }
            OutputFormat::Plain => {
                self.print_search_results_plain(results);
            }
        }
    }

    fn print_search_results_pretty(&self, results: &[ScoredMemory]) {
        if results.is_empty() {
            println!("No memories found.");
            return;
        }

        println!("Found {} memories:\n", results.len());

        for sm in results {
            let id_short = short_id(&sm.memory.id);
            let score_str = format!("[{:.2}]", sm.score);
            let type_str = format!("{:?}", sm.memory.type_);

            if self.use_color {
                println!(
                    "  {} {} {}  {}",
                    score_str.if_supports_color(Stream::Stdout, |text| text.green()),
                    id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                    type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                    sm.memory.summary
                );
            } else {
                println!(
                    "  {} {} {}  {}",
                    score_str, id_short, type_str, sm.memory.summary
                );
            }
        }
    }

    fn print_search_results_plain(&self, results: &[ScoredMemory]) {
        if results.is_empty() {
            println!("No memories found.");
            return;
        }

        println!("Found {} memories:\n", results.len());

        for sm in results {
            let id_short = short_id(&sm.memory.id);
            let score_str = format!("[{:.2}]", sm.score);
            let type_str = format!("{:?}", sm.memory.type_);
            println!(
                "  {} {} {}  {}",
                score_str, id_short, type_str, sm.memory.summary
            );
        }
    }

    /// Print retrieval results in the configured format.
    pub fn print_retrieval_result(&self, result: &RetrievalResult, show_scores: bool) {
        match self.format {
            OutputFormat::Json => {
                let json_output = serde_json::json!({
                    "memories": result.memories.iter().map(|sm| {
                        serde_json::json!({
                            "memory": sm.memory,
                            "score": sm.score,
                        })
                    }).collect::<Vec<_>>(),
                    "total": result.total,
                });
                println!("{}", serde_json::to_string_pretty(&json_output).unwrap());
            }
            OutputFormat::Pretty => {
                self.print_retrieval_result_pretty(result, show_scores);
            }
            OutputFormat::Plain => {
                self.print_retrieval_result_plain(result, show_scores);
            }
        }
    }

    fn print_retrieval_result_pretty(&self, result: &RetrievalResult, show_scores: bool) {
        if result.memories.is_empty() {
            println!("No memories found.");
            return;
        }

        println!(
            "Found {} memories (out of {} total):\n",
            result.memories.len(),
            result.total
        );

        for sm in &result.memories {
            let id_short = short_id(&sm.memory.id);
            let type_str = format!("{:?}", sm.memory.type_);

            if show_scores {
                let score_str = format!("[{:.2}]", sm.score);
                if self.use_color {
                    println!(
                        "  {} {} {}  {}",
                        score_str.if_supports_color(Stream::Stdout, |text| text.green()),
                        id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                        type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                        sm.memory.summary
                    );
                } else {
                    println!(
                        "  {} {} {}  {}",
                        score_str, id_short, type_str, sm.memory.summary
                    );
                }
            } else if self.use_color {
                println!(
                    "  {} {}  {}",
                    id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                    type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                    sm.memory.summary
                );
            } else {
                println!("  {} {}  {}", id_short, type_str, sm.memory.summary);
            }
        }
    }

    fn print_retrieval_result_plain(&self, result: &RetrievalResult, show_scores: bool) {
        if result.memories.is_empty() {
            println!("No memories found.");
            return;
        }

        println!(
            "Found {} memories (out of {} total):\n",
            result.memories.len(),
            result.total
        );

        for sm in &result.memories {
            let id_short = short_id(&sm.memory.id);
            let type_str = format!("{:?}", sm.memory.type_);

            if show_scores {
                let score_str = format!("[{:.2}]", sm.score);
                println!(
                    "  {} {} {}  {}",
                    score_str, id_short, type_str, sm.memory.summary
                );
            } else {
                println!("  {} {}  {}", id_short, type_str, sm.memory.summary);
            }
        }
    }

    /// Print a list of memory index entries in the configured format.
    pub fn print_memory_list(&self, entries: &[IndexFilterable], verbose: bool) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(entries).unwrap());
            }
            OutputFormat::Pretty => {
                self.print_list_pretty(entries, verbose);
            }
            OutputFormat::Plain => {
                self.print_list_plain(entries, verbose);
            }
        }
    }

    fn print_list_pretty(&self, entries: &[IndexFilterable], verbose: bool) {
        if entries.is_empty() {
            println!("No memories found.");
            return;
        }

        for entry in entries {
            let id_short = short_id(&entry.id);
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

            if verbose {
                println!(
                    "    Criticality: {:.2}  Status: {:?}  Visibility: {:?}",
                    entry.criticality, entry.status, entry.visibility
                );
                if !entry.tags.is_empty() {
                    println!("    Tags: {}", entry.tags.join(", "));
                }
            }
        }
    }

    fn print_list_plain(&self, entries: &[IndexFilterable], verbose: bool) {
        if entries.is_empty() {
            println!("No memories found.");
            return;
        }

        for entry in entries {
            let id_short = short_id(&entry.id);
            println!("{} {:?} {}", id_short, entry.type_, entry.summary);

            if verbose {
                println!(
                    "    Criticality: {:.2}  Status: {:?}  Visibility: {:?}",
                    entry.criticality, entry.status, entry.visibility
                );
                if !entry.tags.is_empty() {
                    println!("    Tags: {}", entry.tags.join(", "));
                }
            }
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

    /// Print project info in the configured format.
    pub fn print_project_info(&self, info: &ProjectInfoOutput) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(info).unwrap());
            }
            OutputFormat::Pretty => {
                let id_display = if self.use_color {
                    info.project_id
                        .as_str()
                        .if_supports_color(Stream::Stdout, |text| text.cyan())
                        .to_string()
                } else {
                    info.project_id.clone()
                };
                println!("Project: {}", info.project_name);
                println!("ID: {}", id_display);
                if let Some(parent) = info.parent_project_id.as_deref() {
                    let parent_display = if self.use_color {
                        parent
                            .if_supports_color(Stream::Stdout, |text| text.cyan())
                            .to_string()
                    } else {
                        parent.to_string()
                    };
                    println!("Parent: {}", parent_display);
                }
                println!("Path: {}", info.project_path);
                println!("Memories: {}", info.memory_count);
                if !info.logical_scopes.is_empty() {
                    println!("Scopes: {}", info.logical_scopes.join(", "));
                }
                println!("Created: {}", info.created_at.format("%Y-%m-%d %H:%M:%S"));
            }
            OutputFormat::Plain => {
                println!("Project: {}", info.project_name);
                println!("ID: {}", info.project_id);
                if let Some(parent) = info.parent_project_id.as_deref() {
                    println!("Parent: {}", parent);
                }
                println!("Path: {}", info.project_path);
                println!("Memories: {}", info.memory_count);
                if !info.logical_scopes.is_empty() {
                    println!("Scopes: {}", info.logical_scopes.join(", "));
                }
                println!("Created: {}", info.created_at.format("%Y-%m-%d %H:%M:%S"));
            }
        }
    }

    /// Print a list of projects in the configured format.
    pub fn print_project_list(&self, entries: &[ProjectListOutput]) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(entries).unwrap());
            }
            OutputFormat::Pretty => {
                if entries.is_empty() {
                    println!("No registered projects.");
                    return;
                }
                for entry in entries {
                    let id_short = short_id(&entry.project_id);
                    let id_display = if self.use_color {
                        id_short
                            .if_supports_color(Stream::Stdout, |text| text.cyan())
                            .to_string()
                    } else {
                        id_short.to_string()
                    };
                    let status = if entry.exists {
                        "ok".to_string()
                    } else if self.use_color {
                        "missing"
                            .if_supports_color(Stream::Stdout, |text| text.red())
                            .to_string()
                    } else {
                        "missing".to_string()
                    };
                    let indent = if entry.parent_project_id.is_some() {
                        "  ↳ "
                    } else {
                        ""
                    };
                    println!(
                        "{}{} {} ({})",
                        indent, id_display, entry.project_path, status,
                    );
                    if let Some(parent) = entry.parent_project_id.as_deref() {
                        let parent_short = short_id(parent);
                        let parent_display = if self.use_color {
                            parent_short
                                .if_supports_color(Stream::Stdout, |text| text.dimmed())
                                .to_string()
                        } else {
                            parent_short.to_string()
                        };
                        println!("      parent: {}", parent_display);
                    }
                }
            }
            OutputFormat::Plain => {
                if entries.is_empty() {
                    println!("No registered projects.");
                    return;
                }
                for entry in entries {
                    let id_short = short_id(&entry.project_id);
                    let status = if entry.exists { "ok" } else { "missing" };
                    let prefix = if entry.parent_project_id.is_some() {
                        "  "
                    } else {
                        ""
                    };
                    println!("{}{} {} {}", prefix, id_short, entry.project_path, status,);
                    if let Some(parent) = entry.parent_project_id.as_deref() {
                        println!("    parent: {}", short_id(parent));
                    }
                }
            }
        }
    }

    /// Print aggregate statistics across all projects.
    pub fn print_aggregate_stats(&self, stats: &AggregateStatsOutput) {
        match self.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(stats).unwrap());
            }
            OutputFormat::Pretty => {
                println!("Total Projects: {}", stats.total_projects);
                println!("Reachable: {}", stats.reachable_projects);
                println!("Total Memories: {}", stats.total_memories);
                if !stats.by_type.is_empty() {
                    println!("\nBy Type:");
                    for (type_, count) in &stats.by_type {
                        println!("  {:?}: {}", type_, count);
                    }
                }
            }
            OutputFormat::Plain => {
                println!("Projects: {}", stats.total_projects);
                println!("Reachable: {}", stats.reachable_projects);
                println!("Memories: {}", stats.total_memories);
                for (type_, count) in &stats.by_type {
                    println!("{:?}: {}", type_, count);
                }
            }
        }
    }
}

/// Output data for project info display.
#[derive(Debug, serde::Serialize)]
pub struct ProjectInfoOutput {
    pub project_id: String,
    pub project_name: String,
    pub project_path: String,
    pub memory_count: usize,
    pub logical_scopes: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_project_id: Option<String>,
}

/// Output data for a single project list entry.
#[derive(Debug, serde::Serialize)]
pub struct ProjectListOutput {
    pub project_id: String,
    pub project_path: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_project_id: Option<String>,
}

/// Output data for aggregate stats across projects.
#[derive(Debug, serde::Serialize)]
pub struct AggregateStatsOutput {
    pub total_projects: usize,
    pub reachable_projects: usize,
    pub total_memories: usize,
    pub by_type: Vec<(MemoryType, usize)>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::engine::{RetrievalResult, ScoredMemory};
    use crate::scoring::ScoreBreakdown;
    use crate::types::{Memory, MemoryType, Provenance, Status, Visibility};

    fn test_memory() -> Memory {
        Memory {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            type_: MemoryType::Decision,
            summary: "Test summary".to_string(),
            title: None,
            content: "Test content".to_string(),
            details: None,
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: 0.8,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.9,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            accessed_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    fn test_score_breakdown() -> ScoreBreakdown {
        ScoreBreakdown {
            final_score: 0.75,
            semantic: Some(0.8),
            keyword: None,
            rerank: None,
            relevance: 0.7,
            scope: 0.6,
            scope_multiplier: 0.8,
            trust: 1.0,
            trust_multiplier: 1.0,
            decay: 1.0,
            criticality: 0.8,
        }
    }

    fn test_index_entry() -> IndexFilterable {
        IndexFilterable {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            type_: MemoryType::Decision,
            summary: "Test index entry".to_string(),
            physical: vec![],
            logical: vec![],
            tags: vec![],
            criticality: 0.8,
            status: Status::Active,
            visibility: Visibility::Shared,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    // ========================================
    // 1. short_id helper tests
    // ========================================

    #[test]
    fn test_short_id_normal() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(short_id(uuid), "550e8400-e29b");
    }

    #[test]
    fn test_short_id_short_input() {
        let short_str = "12345";
        assert_eq!(short_id(short_str), "12345");
    }

    #[test]
    fn test_short_id_exact_13() {
        let exact = "1234567890123";
        assert_eq!(short_id(exact), "1234567890123");
    }

    // ========================================
    // 2. Constructor tests
    // ========================================

    #[test]
    fn test_formatter_json_flag_overrides() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Pretty), true, false);
        // Verify JSON format by checking that print_message produces JSON output
        // We can't easily capture stdout, but we can verify the formatter doesn't panic
        formatter.print_message("test");
    }

    #[test]
    fn test_formatter_explicit_format() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Plain), false, false);
        // Verify it doesn't panic with plain format
        formatter.print_message("test");
    }

    // ========================================
    // 3. print_search_results format routing
    // ========================================

    #[test]
    fn test_search_results_json_format() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let memory = test_memory();
        let scored = ScoredMemory {
            memory,
            score: 0.85,
            score_breakdown: test_score_breakdown(),
        };
        let results = vec![scored];

        // Verify it doesn't panic
        formatter.print_search_results(&results);
    }

    #[test]
    fn test_search_results_empty() {
        let formatter_json = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let formatter_pretty = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let formatter_plain = OutputFormatter::new(Some(OutputFormat::Plain), false, false);

        let empty: Vec<ScoredMemory> = vec![];

        // Verify none panic with empty results
        formatter_json.print_search_results(&empty);
        formatter_pretty.print_search_results(&empty);
        formatter_plain.print_search_results(&empty);
    }

    // ========================================
    // 4. print_retrieval_result format routing
    // ========================================

    #[test]
    fn test_retrieval_result_json_format() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let memory = test_memory();
        let scored = ScoredMemory {
            memory,
            score: 0.85,
            score_breakdown: test_score_breakdown(),
        };
        let result = RetrievalResult {
            memories: vec![scored],
            total: 1,
            retrieval_quality: "full".to_string(),
        };

        // Verify it doesn't panic
        formatter.print_retrieval_result(&result, true);
        formatter.print_retrieval_result(&result, false);
    }

    #[test]
    fn test_retrieval_result_empty() {
        let formatter_json = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let formatter_pretty = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let formatter_plain = OutputFormatter::new(Some(OutputFormat::Plain), false, false);

        let empty_result = RetrievalResult {
            memories: vec![],
            total: 0,
            retrieval_quality: "scope_only".to_string(),
        };

        // Verify none panic with empty results
        formatter_json.print_retrieval_result(&empty_result, true);
        formatter_pretty.print_retrieval_result(&empty_result, false);
        formatter_plain.print_retrieval_result(&empty_result, true);
    }

    // ========================================
    // 5. print_memory_list verbose flag
    // ========================================

    #[test]
    fn test_print_memory_list_json_ignores_verbose() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let entries = vec![test_index_entry()];

        // Both verbose=true and verbose=false should produce same output
        // We verify neither panics
        formatter.print_memory_list(&entries, true);
        formatter.print_memory_list(&entries, false);
    }

    #[test]
    fn test_print_memory_list_empty() {
        let formatter_json = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let formatter_pretty = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let formatter_plain = OutputFormatter::new(Some(OutputFormat::Plain), false, false);

        let empty: Vec<IndexFilterable> = vec![];

        // Verify none panic with empty entries
        formatter_json.print_memory_list(&empty, false);
        formatter_pretty.print_memory_list(&empty, true);
        formatter_plain.print_memory_list(&empty, false);
    }
}
