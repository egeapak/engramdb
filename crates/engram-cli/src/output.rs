//! Output formatting for CLI commands.
//!
//! This module provides a unified output formatter that supports multiple output modes:
//! - **Pretty**: Human-friendly output with colors and formatting (for terminals).
//! - **JSON**: Structured JSON output for programmatic parsing.
//! - **Plain**: Simple text output without colors (for non-TTY environments).
//!
//! The formatter automatically detects terminal capabilities and adjusts formatting
//! accordingly.

use engramdb::retrieval::engine::{RetrievalResult, ScoredMemory};
use engramdb::storage::IndexFilterable;
use engramdb::types::{Memory, MemoryType, Status};
use owo_colors::{OwoColorize, Stream};
use serde_json;
use std::io::{self, IsTerminal};

use super::app::OutputFormat;

/// Helper function to truncate IDs to 13 characters.
///
/// Counts `char`s and slices on a char boundary: a byte slice (`&id[..13]`)
/// would panic on IDs containing multibyte characters (IDs normally are
/// UUIDs, but this also renders arbitrary on-disk file stems).
pub fn short_id(id: &str) -> &str {
    match id.char_indices().nth(13) {
        Some((byte_idx, _)) => &id[..byte_idx],
        None => id,
    }
}

/// §5.4 tags: `[fact]`-style class tag only when the class differs from the
/// type default (off-diagonal), and `[invalidated <date>]` when the validity
/// window is closed (visible only when the caller included such memories).
/// A future-dated window end is still valid (mirrors `expires_at`), so it
/// renders as `[invalidates <date>]` — a schedule, not a tombstone.
fn epistemic_tags(
    type_: MemoryType,
    epistemic: engramdb::types::Epistemic,
    invalidated_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut tags = String::new();
    if epistemic != type_.default_epistemic() {
        tags.push_str(&format!(" [{}]", epistemic.as_str()));
    }
    if let Some(t) = invalidated_at {
        let label = if t <= now {
            "invalidated"
        } else {
            "invalidates"
        };
        tags.push_str(&format!(" [{label} {}]", t.format("%Y-%m-%d")));
    }
    tags
}

/// §5.4: the validity metadata the feature teaches users to record must be
/// visible outside `--format json` — premise ("holds because"), watch globs,
/// task binding, window bounds, supersessor, and verification stamp.
fn print_validity_lines(memory: &Memory) {
    if let Some(v) = &memory.valid_while {
        if let Some(premise) = &v.premise {
            println!("Premise: {}", premise);
        }
        if !v.invalidated_by.is_empty() {
            println!("Invalidated by: {}", v.invalidated_by.join(", "));
        }
        if let Some(task) = &v.origin_task {
            println!(
                "Origin task: {} (generality: {})",
                task,
                v.generality.as_str()
            );
        }
        if !v.derived_from.is_empty() {
            println!(
                "Derived from: {}",
                v.derived_from
                    .iter()
                    .map(|id| short_id(id))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if let Some(t) = memory.valid_from {
        println!("Valid from: {}", t.format("%Y-%m-%d %H:%M:%S"));
    }
    if let Some(t) = memory.invalidated_at {
        println!("Invalidated at: {}", t.format("%Y-%m-%d %H:%M:%S"));
    }
    if let Some(sup) = &memory.superseded_by {
        println!("Superseded by: {}", sup);
    }
    if let Some(t) = memory.verified_at {
        println!("Verified: {}", t.format("%Y-%m-%d %H:%M:%S"));
    }
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

    /// Whether output is JSON (machine-consumed; never prompt interactively).
    ///
    /// Command handlers use this to suppress or redirect human-oriented
    /// `println!` chatter that would otherwise corrupt the JSON document on
    /// stdout (finding #7): when this is true, a handler must emit exactly one
    /// JSON value on stdout (sending any human text to stderr).
    pub fn is_json(&self) -> bool {
        matches!(self.format, OutputFormat::Json)
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
    pub fn print_environment_doctor(&self, result: &engramdb::ops::EnvironmentDoctorResult) {
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
                        use engramdb::ops::CheckStatus;

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
                            use engramdb::ops::CheckStatus;

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
        println!(
            "Type: {}{}",
            type_display,
            epistemic_tags(
                memory.type_,
                memory.epistemic,
                memory.invalidated_at,
                chrono::Utc::now()
            )
        );
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
        print_validity_lines(memory);
        println!("Created: {}", memory.created_at.format("%Y-%m-%d %H:%M:%S"));
        println!("Updated: {}", memory.updated_at.format("%Y-%m-%d %H:%M:%S"));
    }

    fn print_memory_plain(&self, memory: &Memory) {
        println!("ID: {}", memory.id);
        println!(
            "Type: {:?}{}",
            memory.type_,
            epistemic_tags(
                memory.type_,
                memory.epistemic,
                memory.invalidated_at,
                chrono::Utc::now()
            )
        );
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
        print_validity_lines(memory);
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
                        let mut obj = serde_json::json!({
                            "memory": sm.memory,
                            "score": sm.score,
                        });
                        // Parity with MCP query output: expose the component
                        // breakdown (incl. situation_multiplier) when scores
                        // were requested, so profile tuning is observable
                        // from the CLI too.
                        if show_scores {
                            obj["breakdown"] = serde_json::json!(sm.score_breakdown);
                        }
                        obj
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

        let now = chrono::Utc::now();
        for sm in &result.memories {
            let id_short = short_id(&sm.memory.id);
            let type_str = format!("{:?}", sm.memory.type_);
            let tags = epistemic_tags(
                sm.memory.type_,
                sm.memory.epistemic,
                sm.memory.invalidated_at,
                now,
            );

            if show_scores {
                let score_str = format!("[{:.2}]", sm.score);
                if self.use_color {
                    println!(
                        "  {} {} {}{}  {}",
                        score_str.if_supports_color(Stream::Stdout, |text| text.green()),
                        id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                        type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                        tags,
                        sm.memory.summary
                    );
                } else {
                    println!(
                        "  {} {} {}{}  {}",
                        score_str, id_short, type_str, tags, sm.memory.summary
                    );
                }
            } else if self.use_color {
                println!(
                    "  {} {}{}  {}",
                    id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                    type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                    tags,
                    sm.memory.summary
                );
            } else {
                println!("  {} {}{}  {}", id_short, type_str, tags, sm.memory.summary);
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

        let now = chrono::Utc::now();
        for sm in &result.memories {
            let id_short = short_id(&sm.memory.id);
            let type_str = format!("{:?}", sm.memory.type_);
            let tags = epistemic_tags(
                sm.memory.type_,
                sm.memory.epistemic,
                sm.memory.invalidated_at,
                now,
            );

            if show_scores {
                let score_str = format!("[{:.2}]", sm.score);
                println!(
                    "  {} {} {}{}  {}",
                    score_str, id_short, type_str, tags, sm.memory.summary
                );
            } else {
                println!("  {} {}{}  {}", id_short, type_str, tags, sm.memory.summary);
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

            println!(
                "{} {}{} {}",
                id_display,
                type_display,
                epistemic_tags(
                    entry.type_,
                    entry.epistemic,
                    entry.invalidated_at,
                    chrono::Utc::now()
                ),
                entry.summary
            );

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
            println!(
                "{} {:?}{} {}",
                id_short,
                entry.type_,
                epistemic_tags(
                    entry.type_,
                    entry.epistemic,
                    entry.invalidated_at,
                    chrono::Utc::now()
                ),
                entry.summary
            );

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

        if let Some(rt) = &stats.runtime {
            print_runtime_pretty(rt);
        }
    }

    fn print_stats_plain(&self, stats: &Stats) {
        println!("Total: {}", stats.total);
        for (type_, count) in &stats.by_type {
            println!("{:?}: {}", type_, count);
        }
        if let Some(rt) = &stats.runtime {
            println!("Calls: {}", rt.view.usage.total_calls);
            if rt.view.queries.total > 0 {
                println!("Hit rate: {:.3}", rt.view.queries.hit_rate);
            }
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

/// Pretty-print the runtime telemetry overlay below the static stats block.
fn print_runtime_pretty(rt: &engramdb::telemetry::RuntimeSnapshot) {
    println!(
        "\nRuntime telemetry (since {}, project {}):",
        rt.since.format("%Y-%m-%d %H:%M:%S UTC"),
        rt.project_id
    );
    println!("  Total calls: {}", rt.view.usage.total_calls);
    if !rt.view.usage.by_tool.is_empty() {
        println!("  By tool:");
        for (tool, count) in &rt.view.usage.by_tool {
            let errors = rt.view.usage.errors_by_tool.get(tool).copied().unwrap_or(0);
            if errors > 0 {
                println!("    {}: {} ({} errors)", tool, count, errors);
            } else {
                println!("    {}: {}", tool, count);
            }
        }
    }
    if rt.view.queries.total > 0 {
        println!(
            "  Queries: {} (hits: {}, zero-result: {}, hit rate: {:.3})",
            rt.view.queries.total,
            rt.view.queries.hits,
            rt.view.queries.zero_results,
            rt.view.queries.hit_rate
        );
        if !rt.view.queries.by_quality.is_empty() {
            print!("    Quality:");
            for (label, count) in &rt.view.queries.by_quality {
                print!(" {}={}", label, count);
            }
            println!();
        }
    }
    if !rt.view.timings_ms.tool.is_empty() {
        println!("  Tool timings (ms):");
        for (tool, t) in &rt.view.timings_ms.tool {
            println!(
                "    {}: avg {:.1}, p50 {:.1}, p95 {:.1} (n={})",
                tool, t.avg, t.p50, t.p95, t.count
            );
        }
    }
    if !rt.view.timings_ms.stages.is_empty() {
        println!("  Stage timings (ms):");
        for (stage, t) in &rt.view.timings_ms.stages {
            println!(
                "    {}: avg {:.1}, p50 {:.1}, p95 {:.1} (n={})",
                stage, t.avg, t.p50, t.p95, t.count
            );
        }
    }
    if let Some(by_project) = &rt.by_project {
        println!("  By project ({} project(s)):", by_project.len());
        for (pid, view) in by_project {
            println!(
                "    {}: {} calls, {} queries (hit rate {:.3})",
                pid, view.usage.total_calls, view.queries.total, view.queries.hit_rate
            );
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
    /// Optional runtime telemetry (per-project usage counters, hit-rate,
    /// response timings). Populated from the persisted `stats.json`
    /// snapshot for the current project, or `None` if no telemetry has
    /// been recorded yet.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<engramdb::telemetry::RuntimeSnapshot>,
}

/// Format the ping statistics line for `daemon status` output.
///
/// When `last_ping_secs_ago` is `Some(n)`, produces `"pings: {count} (last {n}s ago)"`.
/// When `None` (no ping received yet in this daemon's lifetime), produces `"pings: {count}"`.
pub fn format_ping_line(ping_count: u64, last_ping_secs_ago: Option<u64>) -> String {
    match last_ping_secs_ago {
        Some(n) => format!("pings: {} (last {}s ago)", ping_count, n),
        None => format!("pings: {}", ping_count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::retrieval::engine::{RetrievalResult, ScoredMemory};
    use engramdb::scoring::ScoreBreakdown;
    use engramdb::types::{Memory, MemoryType, Provenance, Status, Visibility};

    fn test_memory() -> Memory {
        Memory {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            type_: MemoryType::Decision,
            epistemic: MemoryType::Decision.default_epistemic(),
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
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
            situation_multiplier: 1.0,
            decay: 1.0,
            criticality: 0.8,
        }
    }

    fn test_index_entry() -> IndexFilterable {
        IndexFilterable {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            type_: MemoryType::Decision,
            epistemic: MemoryType::Decision.default_epistemic(),
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
            valid_from: None,
            invalidated_at: None,
        }
    }

    // ========================================
    // 0. epistemic_tags helper tests (§5.4)
    // ========================================

    #[test]
    fn test_epistemic_tags_diagonal_is_empty() {
        let tags = epistemic_tags(
            MemoryType::Decision,
            MemoryType::Decision.default_epistemic(),
            None,
            chrono::Utc::now(),
        );
        assert_eq!(tags, "");
    }

    #[test]
    fn test_epistemic_tags_off_diagonal_and_invalidated() {
        use chrono::TimeZone;
        let when = chrono::Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 19, 0, 0, 0).unwrap();
        let tags = epistemic_tags(
            MemoryType::Context,
            engramdb::types::Epistemic::Observation,
            Some(when),
            now,
        );
        assert_eq!(tags, " [observation] [invalidated 2026-07-01]");
    }

    /// A future-dated window end is still valid (mirrors `expires_at`), so
    /// it must not read as a tombstone.
    #[test]
    fn test_epistemic_tags_future_invalidation_is_schedule_not_tombstone() {
        use chrono::TimeZone;
        let when = chrono::Utc.with_ymd_and_hms(2026, 8, 18, 0, 0, 0).unwrap();
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 19, 0, 0, 0).unwrap();
        let tags = epistemic_tags(
            MemoryType::Decision,
            MemoryType::Decision.default_epistemic(),
            Some(when),
            now,
        );
        assert_eq!(tags, " [invalidates 2026-08-18]");
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

    #[test]
    fn test_short_id_multibyte_does_not_panic() {
        // Non-UUID ids can come from arbitrary on-disk file stems. A byte
        // slice at index 13 would land mid-codepoint here and panic.
        let multibyte = "ééééééééééééééé"; // 15 chars, 2 bytes each
        assert_eq!(short_id(multibyte), "ééééééééééééé"); // first 13 chars

        let emoji = "🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀"; // 14 chars, 4 bytes each
        assert_eq!(short_id(emoji).chars().count(), 13);

        // Shorter-than-13 multibyte input is returned whole.
        assert_eq!(short_id("héllo"), "héllo");
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

    // ========================================
    // 6. JSON serialization assertions
    //
    // The existing print_* tests above only assert "doesn't panic"; the JSON
    // format branch is `serde_json::to_string_pretty(...)` so we can assert
    // the actual shape via serde without needing stdout capture. These tests
    // lock down the public JSON contract — clients (LLM agents, scripts)
    // parsing this output should not silently break on field rename / removal.
    // ========================================

    fn test_environment_doctor_result() -> engramdb::ops::EnvironmentDoctorResult {
        use engramdb::ops::{DoctorSection, EnvironmentCheck};
        engramdb::ops::EnvironmentDoctorResult {
            sections: vec![DoctorSection {
                name: "System".to_string(),
                checks: vec![EnvironmentCheck {
                    name: "binary".to_string(),
                    passed: true,
                    message: "ok".to_string(),
                    suggestion: None,
                    details: vec![],
                    status: None,
                }],
                subsections: vec![],
            }],
            all_passed: true,
            store_check: None,
        }
    }

    #[test]
    fn environment_doctor_json_round_trips() {
        // The JSON branch of print_environment_doctor uses
        // serde_json::to_string_pretty(result). Lock the field names down so
        // any client parsing `doctor --json` keeps working across renames.
        let result = test_environment_doctor_result();
        let v = serde_json::to_value(&result).unwrap();
        assert!(v.get("sections").is_some(), "must serialize 'sections'");
        assert_eq!(v["all_passed"], serde_json::Value::Bool(true));
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["name"], "System");
        let checks = sections[0]["checks"].as_array().unwrap();
        assert_eq!(checks[0]["name"], "binary");
        assert_eq!(checks[0]["passed"], serde_json::Value::Bool(true));
        assert_eq!(checks[0]["message"], "ok");
        // skip_serializing_if attributes hold: optional fields are absent.
        assert!(checks[0].get("suggestion").is_none());
        assert!(checks[0].get("details").is_none());
        assert!(checks[0].get("status").is_none());
    }

    #[test]
    fn environment_check_status_serializes_snake_case() {
        // The status enum carries #[serde(rename_all = "snake_case")].
        // If that ever changes, every JSON consumer dispatching on this
        // field breaks silently. Pin the on-wire form.
        use engramdb::ops::{CheckStatus, EnvironmentCheck};
        let check = EnvironmentCheck {
            name: "n".to_string(),
            passed: false,
            message: "m".to_string(),
            suggestion: Some("try X".to_string()),
            details: vec!["d1".to_string()],
            status: Some(CheckStatus::Warn),
        };
        let v = serde_json::to_value(&check).unwrap();
        assert_eq!(v["status"], "warn");
        assert_eq!(v["suggestion"], "try X");
        assert_eq!(v["details"], serde_json::json!(["d1"]));
    }

    #[test]
    fn project_info_output_json_includes_required_fields() {
        let info = ProjectInfoOutput {
            project_id: "pid-123".to_string(),
            project_name: "demo".to_string(),
            project_path: "/tmp/demo".to_string(),
            memory_count: 7,
            logical_scopes: vec!["db".to_string(), "ui".to_string()],
            created_at: chrono::Utc::now(),
            parent_project_id: None,
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["project_id"], "pid-123");
        assert_eq!(v["project_name"], "demo");
        assert_eq!(v["memory_count"], 7);
        assert_eq!(v["logical_scopes"], serde_json::json!(["db", "ui"]));
        // parent_project_id is skipped when None.
        assert!(v.get("parent_project_id").is_none());
    }

    #[test]
    fn project_info_output_includes_parent_when_set() {
        let info = ProjectInfoOutput {
            project_id: "child".to_string(),
            project_name: "demo".to_string(),
            project_path: "/tmp/demo".to_string(),
            memory_count: 0,
            logical_scopes: vec![],
            created_at: chrono::Utc::now(),
            parent_project_id: Some("parent-pid".to_string()),
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["parent_project_id"], "parent-pid");
    }

    #[test]
    fn project_list_output_json_round_trip() {
        let entries = vec![
            ProjectListOutput {
                project_id: "a".to_string(),
                project_path: "/p/a".to_string(),
                exists: true,
                parent_project_id: None,
            },
            ProjectListOutput {
                project_id: "b".to_string(),
                project_path: "/p/b".to_string(),
                exists: false,
                parent_project_id: Some("a".to_string()),
            },
        ];
        let v = serde_json::to_value(&entries).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["exists"], serde_json::Value::Bool(true));
        assert!(arr[0].get("parent_project_id").is_none());
        assert_eq!(arr[1]["exists"], serde_json::Value::Bool(false));
        assert_eq!(arr[1]["parent_project_id"], "a");
    }

    #[test]
    fn aggregate_stats_output_json_round_trip() {
        let stats = AggregateStatsOutput {
            total_projects: 3,
            reachable_projects: 2,
            total_memories: 42,
            by_type: vec![(MemoryType::Decision, 30), (MemoryType::Hazard, 12)],
        };
        let v = serde_json::to_value(&stats).unwrap();
        assert_eq!(v["total_projects"], 3);
        assert_eq!(v["reachable_projects"], 2);
        assert_eq!(v["total_memories"], 42);
        let by_type = v["by_type"].as_array().unwrap();
        assert_eq!(by_type.len(), 2);
        // by_type is `Vec<(MemoryType, usize)>` — serializes as `[[..., 30], [..., 12]]`.
        assert_eq!(by_type[0][1], 30);
    }

    #[test]
    fn stats_json_includes_core_fields() {
        let stats = Stats {
            total: 10,
            by_type: vec![(MemoryType::Decision, 4)],
            by_status: vec![(Status::Active, 8), (Status::Challenged, 2)],
            by_scope: vec![("api".to_string(), 3)],
            expired: 1,
            oldest: None,
            newest: None,
            avg_criticality: 0.62,
            runtime: None,
        };
        let v = serde_json::to_value(&stats).unwrap();
        assert_eq!(v["total"], 10);
        assert_eq!(v["expired"], 1);
        // f64 equality via serde — within float epsilon.
        let avg = v["avg_criticality"].as_f64().unwrap();
        assert!((avg - 0.62).abs() < 1e-9);
        assert_eq!(v["by_status"].as_array().unwrap().len(), 2);
    }

    /// `no_color=true` must produce a formatter that doesn't emit ANSI
    /// escapes — relevant for piped output and CI logs. We can't observe
    /// stdout here without a refactor, but we can lock the internal flag.
    #[test]
    fn formatter_no_color_disables_color() {
        let f = OutputFormatter::new(Some(OutputFormat::Pretty), false, true);
        assert!(!f.use_color, "no_color must zero out use_color");
    }

    /// JSON format mode forces use_color off (colors don't apply to JSON).
    #[test]
    fn formatter_json_mode_has_no_color() {
        let f = OutputFormatter::new(None, true, false);
        assert!(matches!(f.format, OutputFormat::Json));
        assert!(!f.use_color, "JSON mode must never use color");
    }

    // ========================================
    // 7. print_environment_doctor pretty/plain rendering
    //
    // Builds a maximally exhaustive EnvironmentDoctorResult — pass, fail,
    // warn, info checks, with/without suggestions, with details, with a
    // subsection — and drives the formatter through Pretty, Plain, and
    // colorless variants. These tests don't capture stdout (the formatter
    // writes via println!) so they're asserting "covers every branch
    // without panicking". The pre-existing JSON test still locks the wire
    // contract on top of this.
    // ========================================

    fn doctor_result_with_all_statuses() -> engramdb::ops::EnvironmentDoctorResult {
        use engramdb::ops::doctor::DoctorSubSection;
        use engramdb::ops::{
            CheckStatus, DoctorSection, EnvironmentCheck, EnvironmentDoctorResult,
        };
        EnvironmentDoctorResult {
            sections: vec![
                DoctorSection {
                    name: "System".to_string(),
                    checks: vec![
                        EnvironmentCheck {
                            name: "pass-check".to_string(),
                            passed: true,
                            message: "ok".to_string(),
                            suggestion: None,
                            details: vec![],
                            status: Some(CheckStatus::Pass),
                        },
                        EnvironmentCheck {
                            name: "fail-check".to_string(),
                            passed: false,
                            message: "broken".to_string(),
                            suggestion: Some("try the fix".to_string()),
                            details: vec!["line 1".to_string(), "line 2".to_string()],
                            status: Some(CheckStatus::Fail),
                        },
                        EnvironmentCheck {
                            name: "warn-check".to_string(),
                            passed: false,
                            message: "soft warning".to_string(),
                            suggestion: None,
                            details: vec![],
                            status: Some(CheckStatus::Warn),
                        },
                        EnvironmentCheck {
                            name: "info-check".to_string(),
                            passed: true,
                            message: "informational".to_string(),
                            suggestion: None,
                            details: vec![],
                            status: Some(CheckStatus::Info),
                        },
                        // status: None + passed: true → icon resolved from `passed`
                        EnvironmentCheck {
                            name: "implicit-pass".to_string(),
                            passed: true,
                            message: "implicit".to_string(),
                            suggestion: None,
                            details: vec![],
                            status: None,
                        },
                        EnvironmentCheck {
                            name: "implicit-fail".to_string(),
                            passed: false,
                            message: "implicit fail".to_string(),
                            suggestion: None,
                            details: vec![],
                            status: None,
                        },
                    ],
                    subsections: vec![DoctorSubSection {
                        name: "Sub group".to_string(),
                        checks: vec![
                            EnvironmentCheck {
                                name: "sub-pass".to_string(),
                                passed: true,
                                message: "fine".to_string(),
                                suggestion: None,
                                details: vec![],
                                status: Some(CheckStatus::Pass),
                            },
                            EnvironmentCheck {
                                name: "sub-warn".to_string(),
                                passed: false,
                                message: "watch out".to_string(),
                                suggestion: Some("look here".to_string()),
                                details: vec!["dim line".to_string()],
                                status: Some(CheckStatus::Warn),
                            },
                            EnvironmentCheck {
                                name: "sub-info".to_string(),
                                passed: true,
                                message: "fyi".to_string(),
                                suggestion: None,
                                details: vec![],
                                status: Some(CheckStatus::Info),
                            },
                            EnvironmentCheck {
                                name: "sub-fail".to_string(),
                                passed: false,
                                message: "down".to_string(),
                                suggestion: None,
                                details: vec![],
                                status: Some(CheckStatus::Fail),
                            },
                        ],
                    }],
                },
                // Second section with only a passed check — exercises the
                // section-loop with multiple sections.
                DoctorSection {
                    name: "Other".to_string(),
                    checks: vec![EnvironmentCheck {
                        name: "trivial".to_string(),
                        passed: true,
                        message: "trivial ok".to_string(),
                        suggestion: None,
                        details: vec![],
                        status: None,
                    }],
                    subsections: vec![],
                },
            ],
            all_passed: false,
            store_check: None,
        }
    }

    #[test]
    fn print_environment_doctor_pretty_with_color_covers_all_statuses() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let result = doctor_result_with_all_statuses();
        formatter.print_environment_doctor(&result);
    }

    #[test]
    fn print_environment_doctor_pretty_without_color_covers_all_statuses() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Pretty), false, true);
        let result = doctor_result_with_all_statuses();
        formatter.print_environment_doctor(&result);
    }

    #[test]
    fn print_environment_doctor_plain_covers_all_statuses() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Plain), false, false);
        let result = doctor_result_with_all_statuses();
        formatter.print_environment_doctor(&result);
    }

    #[test]
    fn print_environment_doctor_json_serializes_full_tree() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let result = doctor_result_with_all_statuses();
        // Drives the JSON branch (cli/output.rs:147).
        formatter.print_environment_doctor(&result);
        // Also assert the underlying serialization shape so the JSON
        // branch's wire contract stays locked.
        let v = serde_json::to_value(&result).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0]["subsections"].as_array().unwrap().len(), 1);
        // Multiple status values present.
        let statuses: std::collections::HashSet<_> = sections[0]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.get("status").and_then(|s| s.as_str()))
            .collect();
        assert!(statuses.contains("pass"));
        assert!(statuses.contains("fail"));
        assert!(statuses.contains("warn"));
        assert!(statuses.contains("info"));
    }

    #[test]
    fn print_environment_doctor_empty_sections_does_not_panic() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let result = engramdb::ops::EnvironmentDoctorResult {
            sections: vec![],
            all_passed: true,
            store_check: None,
        };
        formatter.print_environment_doctor(&result);
    }

    // ========================================
    // 8. print_runtime_pretty via print_stats with a hand-built snapshot
    //
    // print_runtime_pretty (cli/output.rs:902) has CRAP 182 and was at 0%
    // coverage — its 5 inner-loop branches (by_tool with/without errors,
    // queries with/without quality bucket, tool/stage timings, by_project
    // overlay) never executed in tests. Construct a Stats that drives every
    // branch and call print_stats. We deliberately route through the public
    // `print_stats` to also nudge `print_stats_pretty`'s "if let Some(rt)"
    // branch (cli/output.rs:731).
    // ========================================

    fn fully_populated_runtime_snapshot() -> engramdb::telemetry::RuntimeSnapshot {
        use engramdb::telemetry::{
            ProjectView, QueriesView, RuntimeSnapshot, TimingStats, TimingsView, UsageView,
        };
        use std::collections::BTreeMap;

        let mut by_tool = BTreeMap::new();
        by_tool.insert("query".to_string(), 12);
        by_tool.insert("create".to_string(), 4);
        let mut errors_by_tool = BTreeMap::new();
        // One tool with errors → "{n} errors" branch, the other without.
        errors_by_tool.insert("query".to_string(), 2);

        let mut by_quality: BTreeMap<&'static str, u64> = BTreeMap::new();
        by_quality.insert("full", 5);
        by_quality.insert("keyword_only", 1);

        let mut tool_timings = BTreeMap::new();
        tool_timings.insert(
            "query".to_string(),
            TimingStats {
                count: 12,
                avg: 42.5,
                p50: 38.0,
                p95: 80.0,
            },
        );
        let mut stage_timings = BTreeMap::new();
        stage_timings.insert(
            "embed".to_string(),
            TimingStats {
                count: 16,
                avg: 8.1,
                p50: 7.0,
                p95: 14.0,
            },
        );

        let view = ProjectView {
            usage: UsageView {
                total_calls: 16,
                unique_sessions: 3,
                by_tool,
                errors_by_tool,
            },
            queries: QueriesView {
                total: 6,
                hits: 5,
                zero_results: 1,
                hit_rate: 0.833,
                followups: 2,
                followup_rate: 0.333,
                by_quality,
            },
            timings_ms: TimingsView {
                tool: tool_timings,
                stages: stage_timings,
            },
        };

        let mut by_project = std::collections::BTreeMap::new();
        by_project.insert("project-a".to_string(), view.clone());
        by_project.insert("project-b".to_string(), ProjectView::default());

        RuntimeSnapshot {
            since: chrono::Utc::now(),
            project_id: "project-a".to_string(),
            persistence_failures: 0,
            view,
            by_project: Some(by_project),
        }
    }

    fn empty_runtime_snapshot() -> engramdb::telemetry::RuntimeSnapshot {
        engramdb::telemetry::RuntimeSnapshot {
            since: chrono::Utc::now(),
            project_id: "project-empty".to_string(),
            persistence_failures: 0,
            view: engramdb::telemetry::ProjectView::default(),
            by_project: None,
        }
    }

    fn stats_with_runtime(rt: engramdb::telemetry::RuntimeSnapshot) -> Stats {
        Stats {
            total: 3,
            by_type: vec![(MemoryType::Decision, 2), (MemoryType::Convention, 1)],
            by_status: vec![(Status::Active, 3)],
            by_scope: vec![("services/api".to_string(), 2)],
            expired: 0,
            oldest: None,
            newest: None,
            avg_criticality: 0.75,
            runtime: Some(rt),
        }
    }

    #[test]
    fn print_runtime_pretty_drives_all_branches_via_stats_pretty() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let stats = stats_with_runtime(fully_populated_runtime_snapshot());
        formatter.print_stats(&stats);
    }

    #[test]
    fn print_runtime_pretty_empty_snapshot_skips_optional_blocks() {
        // No by_tool, no queries, no timings, no by_project → every `if !x.is_empty()`
        // branch in print_runtime_pretty takes the False path.
        let formatter = OutputFormatter::new(Some(OutputFormat::Pretty), false, false);
        let stats = stats_with_runtime(empty_runtime_snapshot());
        formatter.print_stats(&stats);
    }

    #[test]
    fn print_runtime_plain_with_runtime_does_not_panic() {
        // print_stats_plain has its own runtime branch (cli/output.rs:741).
        let formatter = OutputFormatter::new(Some(OutputFormat::Plain), false, false);
        let stats = stats_with_runtime(fully_populated_runtime_snapshot());
        formatter.print_stats(&stats);
    }

    #[test]
    fn print_stats_json_serializes_runtime_payload() {
        let formatter = OutputFormatter::new(Some(OutputFormat::Json), false, false);
        let stats = stats_with_runtime(fully_populated_runtime_snapshot());
        // Lock the JSON output shape — the Stats `runtime` field carries
        // `#[serde(flatten)]`, so RuntimeSnapshot fields appear at the top
        // level of the Stats JSON. Dashboards reading `stats --json` see
        // since/project_id/usage/queries/timings_ms next to the static
        // memory counters.
        let json = serde_json::to_value(&stats).unwrap();
        assert!(json["since"].is_string(), "since must be at top level");
        assert!(
            json["project_id"].is_string(),
            "project_id must be at top level"
        );
        // ProjectView fields are doubly flattened via RuntimeSnapshot.
        assert!(json["usage"].is_object(), "usage must be top-level");
        assert!(json["queries"].is_object(), "queries must be top-level");
        assert!(
            json["timings_ms"].is_object(),
            "timings_ms must be top-level"
        );
        // Static stats fields still present.
        assert_eq!(json["total"], 3);
        // Drive the print path as well so the JSON branch of print_stats
        // executes.
        formatter.print_stats(&stats);
    }

    // ========================================
    // 9. format_ping_line formatter
    // ========================================

    #[test]
    fn format_ping_line_with_last_ping_some() {
        let line = format_ping_line(2, Some(0));
        assert_eq!(line, "pings: 2 (last 0s ago)");
    }

    #[test]
    fn format_ping_line_with_last_ping_nonzero() {
        let line = format_ping_line(42, Some(12));
        assert_eq!(line, "pings: 42 (last 12s ago)");
    }

    #[test]
    fn format_ping_line_with_no_ping_yet() {
        let line = format_ping_line(0, None);
        assert_eq!(line, "pings: 0");
    }

    #[test]
    fn format_ping_line_count_nonzero_no_last() {
        // Should not happen in practice (last_ping is always set when count >
        // 0), but the formatter must not panic.
        let line = format_ping_line(5, None);
        assert_eq!(line, "pings: 5");
    }
}
