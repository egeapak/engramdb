//! Output formatting for CLI commands.
//!
//! This module provides a unified output formatter that supports multiple output modes:
//! - **Pretty**: Human-friendly output with colors and formatting (for terminals).
//! - **JSON**: Structured JSON output for programmatic parsing.
//! - **Plain**: Simple text output without colors (for non-TTY environments).
//!
//! The formatter automatically detects terminal capabilities and adjusts formatting
//! accordingly.

use crate::project_tree::{build_render_model, RenderLine};
use engramdb::retrieval::engine::{RetrievalResult, ScoredMemory};
use engramdb::storage::IndexFilterable;
use engramdb::types::{Memory, MemoryType, ProjectListGrouping, Status};
use owo_colors::{OwoColorize, Stream};
use serde_json;
use std::fmt::Write as _;
use std::io::{self, IsTerminal};
use std::sync::{Arc, Mutex};

use super::app::OutputFormat;

/// Where a rendered line goes.
///
/// Production always uses [`Sink::Stdout`] / [`Sink::Stderr`], which forward to
/// the same `println!` / `eprintln!` this module used to call directly, so
/// buffering, locking and interleaving with the rest of the CLI are unchanged.
///
/// [`Sink::Capture`] exists so tests can read back what a renderer produced.
/// Without it the only observable effect of a `print_*` method is on the
/// process's real stdout, which a unit test cannot inspect — which is why the
/// renderer tests in this file could historically assert nothing stronger than
/// "it did not panic".
///
/// `Arc<Mutex<_>>` rather than `RefCell`: `&OutputFormatter` is held across
/// `.await` points by every async command handler, so the formatter has to
/// stay `Send + Sync`.
enum Sink {
    Stdout,
    Stderr,
    /// Only ever constructed by [`OutputFormatter::capturing`], which is
    /// test-only. The variant stays compiled in either way so that `line` and
    /// `raw` are the same code in a test build and a release build.
    #[cfg_attr(not(test), allow(dead_code))]
    Capture(Arc<Mutex<String>>),
}

impl Sink {
    /// Write `args` followed by a newline.
    fn line(&self, args: std::fmt::Arguments<'_>) {
        match self {
            Sink::Stdout => println!("{}", args),
            Sink::Stderr => eprintln!("{}", args),
            Sink::Capture(buf) => {
                let mut buf = buf.lock().unwrap_or_else(|e| e.into_inner());
                let _ = writeln!(buf, "{}", args);
            }
        }
    }

    /// Write `args` with no trailing newline.
    fn raw(&self, args: std::fmt::Arguments<'_>) {
        match self {
            Sink::Stdout => print!("{}", args),
            Sink::Stderr => eprint!("{}", args),
            Sink::Capture(buf) => {
                let mut buf = buf.lock().unwrap_or_else(|e| e.into_inner());
                let _ = write!(buf, "{}", args);
            }
        }
    }
}

/// `println!` routed through a formatter's stdout sink.
///
/// `outln!(f)` writes a blank line, matching bare `println!()`.
macro_rules! outln {
    ($f:expr) => { $f.out.line(format_args!("")) };
    ($f:expr, $($arg:tt)*) => { $f.out.line(format_args!($($arg)*)) };
}

/// `eprintln!` routed through a formatter's stderr sink.
macro_rules! errln {
    ($f:expr) => { $f.err.line(format_args!("")) };
    ($f:expr, $($arg:tt)*) => { $f.err.line(format_args!($($arg)*)) };
}

/// `print!` (no trailing newline) routed through a formatter's stdout sink.
macro_rules! outraw {
    ($f:expr, $($arg:tt)*) => { $f.out.raw(format_args!($($arg)*)) };
}

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
fn print_validity_lines(f: &OutputFormatter, memory: &Memory) {
    if let Some(v) = &memory.valid_while {
        if let Some(premise) = &v.premise {
            outln!(f, "Premise: {}", premise);
        }
        if !v.invalidated_by.is_empty() {
            outln!(f, "Invalidated by: {}", v.invalidated_by.join(", "));
        }
        if let Some(task) = &v.origin_task {
            outln!(
                f,
                "Origin task: {} (generality: {})",
                task,
                v.generality.as_str()
            );
        }
        if !v.derived_from.is_empty() {
            outln!(
                f,
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
        outln!(f, "Valid from: {}", t.format("%Y-%m-%d %H:%M:%S"));
    }
    if let Some(t) = memory.invalidated_at {
        outln!(f, "Invalidated at: {}", t.format("%Y-%m-%d %H:%M:%S"));
    }
    if let Some(sup) = &memory.superseded_by {
        outln!(f, "Superseded by: {}", sup);
    }
    if let Some(t) = memory.verified_at {
        outln!(f, "Verified: {}", t.format("%Y-%m-%d %H:%M:%S"));
    }
}

/// Output formatter for CLI results.
///
/// Handles formatting and display of command results in different output modes.
/// Automatically detects terminal capabilities and adjusts formatting.
pub struct OutputFormatter {
    format: OutputFormat,
    use_color: bool,
    out: Sink,
    err: Sink,
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

        Self {
            format,
            use_color,
            out: Sink::Stdout,
            err: Sink::Stderr,
        }
    }

    /// A formatter that buffers instead of printing, plus the handle to read
    /// the buffers back.
    ///
    /// `use_color` is false: colour requires a TTY (see [`OutputFormatter::new`]),
    /// so every redirected invocation — and every test — renders uncoloured.
    #[cfg(test)]
    pub(crate) fn capturing(format: OutputFormat) -> (Self, Capture) {
        Self::capturing_inner(format, false)
    }

    /// [`OutputFormatter::capturing`] with colour forced on.
    ///
    /// Kept separate rather than parameterising `capturing` so the existing
    /// uncoloured snapshots keep their exact call site.
    ///
    /// This only clears the formatter's *own* gate. `if_supports_color` then
    /// asks `supports-color` about the real stdout, which under a test runner
    /// is a pipe — so a caller must also wrap the render in
    /// `owo_colors::with_override(true, …)` to get any escapes out.
    #[cfg(test)]
    pub(crate) fn capturing_colored(format: OutputFormat) -> (Self, Capture) {
        Self::capturing_inner(format, true)
    }

    #[cfg(test)]
    fn capturing_inner(format: OutputFormat, use_color: bool) -> (Self, Capture) {
        let out = Arc::new(Mutex::new(String::new()));
        let err = Arc::new(Mutex::new(String::new()));
        let formatter = Self {
            format,
            use_color,
            out: Sink::Capture(Arc::clone(&out)),
            err: Sink::Capture(Arc::clone(&err)),
        };
        (formatter, Capture { out, err })
    }

    /// Whether this render should be styled.
    ///
    /// `use_color` alone is not the answer: it is computed once in
    /// [`OutputFormatter::new`] and excludes only Json, so a renderer that
    /// consults it from a shared Pretty/Plain code path would style Plain
    /// output on a terminal — which `print_project_list` did.
    fn styled(&self) -> bool {
        self.use_color && matches!(self.format, OutputFormat::Pretty)
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
                outln!(self, "{}", serde_json::json!({ "message": message }));
            }
            OutputFormat::Pretty | OutputFormat::Plain => {
                outln!(self, "{}", message);
            }
        }
    }

    /// Print a success message (with green color in pretty mode).
    pub fn print_success(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                outln!(
                    self,
                    "{}",
                    serde_json::json!({ "success": true, "message": message })
                );
            }
            OutputFormat::Pretty => {
                if self.styled() {
                    outln!(
                        self,
                        "{} {}",
                        "✓".if_supports_color(Stream::Stdout, |text| text.green()),
                        message.if_supports_color(Stream::Stdout, |text| text.green())
                    );
                } else {
                    outln!(self, "✓ {}", message);
                }
            }
            OutputFormat::Plain => {
                outln!(self, "{}", message);
            }
        }
    }

    /// Print an error message (with red color in pretty mode).
    pub fn print_error(&self, message: &str) {
        match self.format {
            OutputFormat::Json => {
                errln!(self, "{}", serde_json::json!({ "error": message }));
            }
            OutputFormat::Pretty => {
                if self.styled() {
                    errln!(
                        self,
                        "{} {}",
                        "✗".if_supports_color(Stream::Stderr, |text| text.red()),
                        message.if_supports_color(Stream::Stderr, |text| text.red())
                    );
                } else {
                    errln!(self, "✗ {}", message);
                }
            }
            OutputFormat::Plain => {
                errln!(self, "Error: {}", message);
            }
        }
    }

    /// Print a hint/suggestion message (with blue color in pretty mode).
    pub fn print_hint(&self, message: &str) {
        match self.format {
            OutputFormat::Pretty => {
                if self.styled() {
                    outln!(
                        self,
                        "  {} {}",
                        "ℹ".if_supports_color(Stream::Stdout, |text| text.blue()),
                        message.if_supports_color(Stream::Stdout, |text| text.blue())
                    );
                } else {
                    outln!(self, "  ℹ {}", message);
                }
            }
            OutputFormat::Plain => {
                outln!(self, "  Hint: {}", message);
            }
            OutputFormat::Json => {} // hints are embedded in structured output
        }
    }

    /// Print full environment doctor results organized by section.
    pub fn print_environment_doctor(&self, result: &engramdb::ops::EnvironmentDoctorResult) {
        match self.format {
            OutputFormat::Json => {
                outln!(self, "{}", serde_json::to_string_pretty(result).unwrap());
            }
            OutputFormat::Pretty | OutputFormat::Plain => {
                let header = "EngramDB Environment Check";
                if self.styled() {
                    outln!(
                        self,
                        "\n{}",
                        header.if_supports_color(Stream::Stdout, |text| text.bold())
                    );
                } else {
                    outln!(self, "\n{}", header);
                }

                for section in &result.sections {
                    outln!(self);
                    if self.styled() {
                        outln!(
                            self,
                            "{}",
                            section
                                .name
                                .if_supports_color(Stream::Stdout, |text| text.bold())
                        );
                    } else {
                        outln!(self, "{}", section.name);
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

                        if self.styled() {
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
                                outln!(
                                    self,
                                    "  {} {}: {}",
                                    colored_icon,
                                    check.name.if_supports_color(Stream::Stdout, |t| t.dimmed()),
                                    check
                                        .message
                                        .if_supports_color(Stream::Stdout, |t| t.dimmed()),
                                );
                            } else if style == "warn" {
                                outln!(
                                    self,
                                    "  {} {}: {}",
                                    colored_icon,
                                    check.name.if_supports_color(Stream::Stdout, |t| t.yellow()),
                                    check.message,
                                );
                            } else {
                                outln!(
                                    self,
                                    "  {} {}: {}",
                                    colored_icon,
                                    check.name,
                                    check.message
                                );
                            }
                        } else {
                            outln!(self, "  {} {}: {}", icon, check.name, check.message);
                        }
                        for detail in &check.details {
                            if self.styled() {
                                outln!(
                                    self,
                                    "      {}",
                                    detail.if_supports_color(Stream::Stdout, |text| text.dimmed())
                                );
                            } else {
                                outln!(self, "      {}", detail);
                            }
                        }
                        if let Some(ref suggestion) = check.suggestion {
                            self.print_hint(suggestion);
                        }
                    }

                    for subsection in &section.subsections {
                        if self.styled() {
                            outln!(
                                self,
                                "  {}",
                                subsection
                                    .name
                                    .if_supports_color(Stream::Stdout, |text| text.dimmed())
                            );
                        } else {
                            outln!(self, "  {}", subsection.name);
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

                            if self.styled() {
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
                                    outln!(
                                        self,
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
                                    outln!(
                                        self,
                                        "    {} {}: {}",
                                        colored_icon,
                                        check
                                            .name
                                            .if_supports_color(Stream::Stdout, |t| t.yellow()),
                                        check.message,
                                    );
                                } else {
                                    outln!(
                                        self,
                                        "    {} {}: {}",
                                        colored_icon,
                                        check.name,
                                        check.message
                                    );
                                }
                            } else {
                                outln!(self, "    {} {}: {}", icon, check.name, check.message);
                            }
                            for detail in &check.details {
                                if self.styled() {
                                    outln!(
                                        self,
                                        "        {}",
                                        detail.if_supports_color(Stream::Stdout, |text| {
                                            text.dimmed()
                                        })
                                    );
                                } else {
                                    outln!(self, "        {}", detail);
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
                errln!(self, "{}", serde_json::json!({ "warning": message }));
            }
            OutputFormat::Pretty => {
                if self.styled() {
                    errln!(
                        self,
                        "{} {}",
                        "⚠".if_supports_color(Stream::Stderr, |text| text.yellow()),
                        message.if_supports_color(Stream::Stderr, |text| text.yellow())
                    );
                } else {
                    errln!(self, "Warning: {}", message);
                }
            }
            OutputFormat::Plain => {
                errln!(self, "Warning: {}", message);
            }
        }
    }

    /// Print a single memory in the configured format.
    pub fn print_memory(&self, memory: &Memory) {
        match self.format {
            OutputFormat::Json => {
                outln!(self, "{}", serde_json::to_string_pretty(memory).unwrap());
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
        let id_display = if self.styled() {
            memory
                .id
                .if_supports_color(Stream::Stdout, |text| text.cyan())
                .to_string()
        } else {
            memory.id.clone()
        };

        let type_display = if self.styled() {
            format!("{:?}", memory.type_)
                .if_supports_color(Stream::Stdout, |text| text.yellow())
                .to_string()
        } else {
            format!("{:?}", memory.type_)
        };

        outln!(self, "ID: {}", id_display);
        outln!(
            self,
            "Type: {}{}",
            type_display,
            epistemic_tags(
                memory.type_,
                memory.epistemic,
                memory.invalidated_at,
                chrono::Utc::now()
            )
        );
        outln!(self, "Summary: {}", memory.summary);
        outln!(self, "Content: {}", memory.content);

        if let Some(ref details) = memory.details {
            outln!(self, "Details: {}", details);
        }

        if !memory.physical.is_empty() {
            outln!(self, "Physical: {}", memory.physical.join(", "));
        }

        if !memory.logical.is_empty() {
            outln!(self, "Logical: {}", memory.logical.join(", "));
        }

        if !memory.tags.is_empty() {
            outln!(self, "Tags: {}", memory.tags.join(", "));
        }

        outln!(self, "Criticality: {:.2}", memory.criticality);
        outln!(self, "Confidence: {:.2}", memory.confidence);
        outln!(self, "Status: {:?}", memory.status);
        outln!(self, "Visibility: {:?}", memory.visibility);
        print_validity_lines(self, memory);
        outln!(
            self,
            "Created: {}",
            memory.created_at.format("%Y-%m-%d %H:%M:%S")
        );
        outln!(
            self,
            "Updated: {}",
            memory.updated_at.format("%Y-%m-%d %H:%M:%S")
        );
    }

    fn print_memory_plain(&self, memory: &Memory) {
        outln!(self, "ID: {}", memory.id);
        outln!(
            self,
            "Type: {:?}{}",
            memory.type_,
            epistemic_tags(
                memory.type_,
                memory.epistemic,
                memory.invalidated_at,
                chrono::Utc::now()
            )
        );
        outln!(self, "Summary: {}", memory.summary);
        outln!(self, "Content: {}", memory.content);

        if let Some(ref details) = memory.details {
            outln!(self, "Details: {}", details);
        }

        if !memory.physical.is_empty() {
            outln!(self, "Physical: {}", memory.physical.join(", "));
        }

        if !memory.logical.is_empty() {
            outln!(self, "Logical: {}", memory.logical.join(", "));
        }

        if !memory.tags.is_empty() {
            outln!(self, "Tags: {}", memory.tags.join(", "));
        }

        outln!(self, "Criticality: {:.2}", memory.criticality);
        outln!(self, "Confidence: {:.2}", memory.confidence);
        outln!(self, "Status: {:?}", memory.status);
        outln!(self, "Visibility: {:?}", memory.visibility);
        print_validity_lines(self, memory);
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
                outln!(
                    self,
                    "{}",
                    serde_json::to_string_pretty(&json_output).unwrap()
                );
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
            outln!(self, "No memories found.");
            return;
        }

        outln!(self, "Found {} memories:\n", results.len());

        for sm in results {
            let id_short = short_id(&sm.memory.id);
            let score_str = format!("[{:.2}]", sm.score);
            let type_str = format!("{:?}", sm.memory.type_);

            if self.styled() {
                outln!(
                    self,
                    "  {} {} {}  {}",
                    score_str.if_supports_color(Stream::Stdout, |text| text.green()),
                    id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                    type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                    sm.memory.summary
                );
            } else {
                outln!(
                    self,
                    "  {} {} {}  {}",
                    score_str,
                    id_short,
                    type_str,
                    sm.memory.summary
                );
            }
        }
    }

    fn print_search_results_plain(&self, results: &[ScoredMemory]) {
        if results.is_empty() {
            outln!(self, "No memories found.");
            return;
        }

        outln!(self, "Found {} memories:\n", results.len());

        for sm in results {
            let id_short = short_id(&sm.memory.id);
            let score_str = format!("[{:.2}]", sm.score);
            let type_str = format!("{:?}", sm.memory.type_);
            outln!(
                self,
                "  {} {} {}  {}",
                score_str,
                id_short,
                type_str,
                sm.memory.summary
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
                outln!(
                    self,
                    "{}",
                    serde_json::to_string_pretty(&json_output).unwrap()
                );
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
            outln!(self, "No memories found.");
            return;
        }

        outln!(
            self,
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
                if self.styled() {
                    outln!(
                        self,
                        "  {} {} {}{}  {}",
                        score_str.if_supports_color(Stream::Stdout, |text| text.green()),
                        id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                        type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                        tags,
                        sm.memory.summary
                    );
                } else {
                    outln!(
                        self,
                        "  {} {} {}{}  {}",
                        score_str,
                        id_short,
                        type_str,
                        tags,
                        sm.memory.summary
                    );
                }
            } else if self.styled() {
                outln!(
                    self,
                    "  {} {}{}  {}",
                    id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                    type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                    tags,
                    sm.memory.summary
                );
            } else {
                outln!(
                    self,
                    "  {} {}{}  {}",
                    id_short,
                    type_str,
                    tags,
                    sm.memory.summary
                );
            }
        }
    }

    fn print_retrieval_result_plain(&self, result: &RetrievalResult, show_scores: bool) {
        if result.memories.is_empty() {
            outln!(self, "No memories found.");
            return;
        }

        outln!(
            self,
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
                outln!(
                    self,
                    "  {} {} {}{}  {}",
                    score_str,
                    id_short,
                    type_str,
                    tags,
                    sm.memory.summary
                );
            } else {
                outln!(
                    self,
                    "  {} {}{}  {}",
                    id_short,
                    type_str,
                    tags,
                    sm.memory.summary
                );
            }
        }
    }

    /// Print a list of memory index entries in the configured format.
    pub fn print_memory_list(&self, entries: &[IndexFilterable], verbose: bool) {
        match self.format {
            OutputFormat::Json => {
                outln!(self, "{}", serde_json::to_string_pretty(entries).unwrap());
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
            outln!(self, "No memories found.");
            return;
        }

        for entry in entries {
            let id_short = short_id(&entry.id);
            let id_display = if self.styled() {
                id_short
                    .if_supports_color(Stream::Stdout, |text| text.cyan())
                    .to_string()
            } else {
                id_short.to_string()
            };

            let type_display = if self.styled() {
                format!("{:?}", entry.type_)
                    .if_supports_color(Stream::Stdout, |text| text.yellow())
                    .to_string()
            } else {
                format!("{:?}", entry.type_)
            };

            outln!(
                self,
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
                outln!(
                    self,
                    "    Criticality: {:.2}  Status: {:?}  Visibility: {:?}",
                    entry.criticality,
                    entry.status,
                    entry.visibility
                );
                if !entry.tags.is_empty() {
                    outln!(self, "    Tags: {}", entry.tags.join(", "));
                }
            }
        }
    }

    fn print_list_plain(&self, entries: &[IndexFilterable], verbose: bool) {
        if entries.is_empty() {
            outln!(self, "No memories found.");
            return;
        }

        for entry in entries {
            let id_short = short_id(&entry.id);
            outln!(
                self,
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
                outln!(
                    self,
                    "    Criticality: {:.2}  Status: {:?}  Visibility: {:?}",
                    entry.criticality,
                    entry.status,
                    entry.visibility
                );
                if !entry.tags.is_empty() {
                    outln!(self, "    Tags: {}", entry.tags.join(", "));
                }
            }
        }
    }

    /// Print statistics in the configured format.
    pub fn print_stats(&self, stats: &Stats) {
        match self.format {
            OutputFormat::Json => {
                outln!(self, "{}", serde_json::to_string_pretty(stats).unwrap());
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
        outln!(self, "Total Memories: {}", stats.total);
        outln!(self, "\nBy Type:");
        for (type_, count) in &stats.by_type {
            outln!(self, "  {:?}: {}", type_, count);
        }
        outln!(self, "\nBy Status:");
        for (status, count) in &stats.by_status {
            outln!(self, "  {:?}: {}", status, count);
        }
        if !stats.by_scope.is_empty() {
            outln!(self, "\nBy Scope:");
            for (scope, count) in &stats.by_scope {
                outln!(self, "  {}: {}", scope, count);
            }
        }
        outln!(self, "\nExpired: {}", stats.expired);
        if let Some(oldest) = stats.oldest {
            outln!(self, "Oldest: {}", oldest.format("%Y-%m-%d"));
        }
        if let Some(newest) = stats.newest {
            outln!(self, "Newest: {}", newest.format("%Y-%m-%d"));
        }
        outln!(self, "\nAverage Criticality: {:.2}", stats.avg_criticality);

        if let Some(rt) = &stats.runtime {
            print_runtime_pretty(self, rt);
        }
    }

    fn print_stats_plain(&self, stats: &Stats) {
        outln!(self, "Total: {}", stats.total);
        for (type_, count) in &stats.by_type {
            outln!(self, "{:?}: {}", type_, count);
        }
        if let Some(rt) = &stats.runtime {
            outln!(self, "Calls: {}", rt.view.usage.total_calls);
            if rt.view.queries.total > 0 {
                outln!(self, "Hit rate: {:.3}", rt.view.queries.hit_rate);
            }
        }
    }

    /// Print project info in the configured format.
    pub fn print_project_info(&self, info: &ProjectInfoOutput) {
        match self.format {
            OutputFormat::Json => {
                outln!(self, "{}", serde_json::to_string_pretty(info).unwrap());
            }
            OutputFormat::Pretty => {
                let id_display = if self.styled() {
                    info.project_id
                        .as_str()
                        .if_supports_color(Stream::Stdout, |text| text.cyan())
                        .to_string()
                } else {
                    info.project_id.clone()
                };
                outln!(self, "Project: {}", info.project_name);
                outln!(self, "ID: {}", id_display);
                if let Some(parent) = info.parent_project_id.as_deref() {
                    let parent_display = if self.styled() {
                        parent
                            .if_supports_color(Stream::Stdout, |text| text.cyan())
                            .to_string()
                    } else {
                        parent.to_string()
                    };
                    outln!(self, "Parent: {}", parent_display);
                }
                outln!(self, "Path: {}", info.project_path);
                outln!(self, "Memories: {}", info.memory_count);
                if !info.logical_scopes.is_empty() {
                    outln!(self, "Scopes: {}", info.logical_scopes.join(", "));
                }
                outln!(
                    self,
                    "Created: {}",
                    info.created_at.format("%Y-%m-%d %H:%M:%S")
                );
            }
            OutputFormat::Plain => {
                outln!(self, "Project: {}", info.project_name);
                outln!(self, "ID: {}", info.project_id);
                if let Some(parent) = info.parent_project_id.as_deref() {
                    outln!(self, "Parent: {}", parent);
                }
                outln!(self, "Path: {}", info.project_path);
                outln!(self, "Memories: {}", info.memory_count);
                if !info.logical_scopes.is_empty() {
                    outln!(self, "Scopes: {}", info.logical_scopes.join(", "));
                }
                outln!(
                    self,
                    "Created: {}",
                    info.created_at.format("%Y-%m-%d %H:%M:%S")
                );
            }
        }
    }

    /// Print a list of projects in the configured format.
    ///
    /// Pretty/Plain render a real tree: worktree sub-projects nest under their
    /// actual parent, grouped under filesystem-directory headers per
    /// `grouping`. JSON stays a flat array (with `parent_project_id`) so
    /// scripts and the MCP surface keep a stable shape regardless of grouping.
    ///
    /// Pretty and Plain deliberately share one layout — the tree is the point
    /// of both — so the only difference is styling, which
    /// [`OutputFormatter::styled`] withholds from Plain.
    pub fn print_project_list(&self, entries: &[ProjectListOutput], grouping: ProjectListGrouping) {
        if let OutputFormat::Json = self.format {
            outln!(self, "{}", serde_json::to_string_pretty(entries).unwrap());
            return;
        }

        if entries.is_empty() {
            outln!(self, "No registered projects.");
            return;
        }

        for line in build_render_model(entries, grouping) {
            outln!(self, "{}", self.render_project_line(&line));
        }
    }

    /// Render one [`RenderLine`] to a styled (Pretty) or plain string.
    fn render_project_line(&self, line: &RenderLine) -> String {
        match line {
            RenderLine::Blank => String::new(),
            RenderLine::Header(dir) => {
                if self.styled() {
                    dir.if_supports_color(Stream::Stdout, |t| t.dimmed())
                        .to_string()
                } else {
                    dir.clone()
                }
            }
            RenderLine::Project {
                project_id,
                depth,
                under_header,
                label,
                exists,
            } => {
                // Header rows indent 2; worktree children add 2 per level and a
                // `↳` marker. Inline/none rows start at column 0.
                let base = if *under_header { 2 } else { 0 };
                let spaces = " ".repeat(base + depth * 2);
                let marker = if *depth > 0 { "↳ " } else { "" };

                let id_short = short_id(project_id);
                let id_display = if self.styled() {
                    id_short
                        .if_supports_color(Stream::Stdout, |t| t.cyan())
                        .to_string()
                } else {
                    id_short.to_string()
                };
                let status = if *exists {
                    "ok".to_string()
                } else if self.styled() {
                    "missing"
                        .if_supports_color(Stream::Stdout, |t| t.red())
                        .to_string()
                } else {
                    "missing".to_string()
                };
                format!("{spaces}{marker}{id_display} {label} ({status})")
            }
        }
    }

    /// Print aggregate statistics across all projects.
    pub fn print_aggregate_stats(&self, stats: &AggregateStatsOutput) {
        match self.format {
            OutputFormat::Json => {
                outln!(self, "{}", serde_json::to_string_pretty(stats).unwrap());
            }
            OutputFormat::Pretty => {
                outln!(self, "Total Projects: {}", stats.total_projects);
                outln!(self, "Reachable: {}", stats.reachable_projects);
                outln!(self, "Total Memories: {}", stats.total_memories);
                if !stats.by_type.is_empty() {
                    outln!(self, "\nBy Type:");
                    for (type_, count) in &stats.by_type {
                        outln!(self, "  {:?}: {}", type_, count);
                    }
                }
            }
            OutputFormat::Plain => {
                outln!(self, "Projects: {}", stats.total_projects);
                outln!(self, "Reachable: {}", stats.reachable_projects);
                outln!(self, "Memories: {}", stats.total_memories);
                for (type_, count) in &stats.by_type {
                    outln!(self, "{:?}: {}", type_, count);
                }
            }
        }
    }
}

/// Pretty-print the runtime telemetry overlay below the static stats block.
fn print_runtime_pretty(f: &OutputFormatter, rt: &engramdb::telemetry::RuntimeSnapshot) {
    outln!(
        f,
        "\nRuntime telemetry (since {}, project {}):",
        rt.since.format("%Y-%m-%d %H:%M:%S UTC"),
        rt.project_id
    );
    outln!(f, "  Total calls: {}", rt.view.usage.total_calls);
    if !rt.view.usage.by_tool.is_empty() {
        outln!(f, "  By tool:");
        for (tool, count) in &rt.view.usage.by_tool {
            let errors = rt.view.usage.errors_by_tool.get(tool).copied().unwrap_or(0);
            if errors > 0 {
                outln!(f, "    {}: {} ({} errors)", tool, count, errors);
            } else {
                outln!(f, "    {}: {}", tool, count);
            }
        }
    }
    if rt.view.queries.total > 0 {
        outln!(
            f,
            "  Queries: {} (hits: {}, zero-result: {}, hit rate: {:.3})",
            rt.view.queries.total,
            rt.view.queries.hits,
            rt.view.queries.zero_results,
            rt.view.queries.hit_rate
        );
        if !rt.view.queries.by_quality.is_empty() {
            outraw!(f, "    Quality:");
            for (label, count) in &rt.view.queries.by_quality {
                outraw!(f, " {}={}", label, count);
            }
            outln!(f);
        }
    }
    if !rt.view.timings_ms.tool.is_empty() {
        outln!(f, "  Tool timings (ms):");
        for (tool, t) in &rt.view.timings_ms.tool {
            outln!(
                f,
                "    {}: avg {:.1}, p50 {:.1}, p95 {:.1} (n={})",
                tool,
                t.avg,
                t.p50,
                t.p95,
                t.count
            );
        }
    }
    if !rt.view.timings_ms.stages.is_empty() {
        outln!(f, "  Stage timings (ms):");
        for (stage, t) in &rt.view.timings_ms.stages {
            outln!(
                f,
                "    {}: avg {:.1}, p50 {:.1}, p95 {:.1} (n={})",
                stage,
                t.avg,
                t.p50,
                t.p95,
                t.count
            );
        }
    }
    if let Some(by_project) = &rt.by_project {
        outln!(f, "  By project ({} project(s)):", by_project.len());
        for (pid, view) in by_project {
            outln!(
                f,
                "    {}: {} calls, {} queries (hit rate {:.3})",
                pid,
                view.usage.total_calls,
                view.queries.total,
                view.queries.hit_rate
            );
        }
    }
}

/// Read access to what a [`OutputFormatter::capturing`] formatter buffered.
#[cfg(test)]
pub(crate) struct Capture {
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
}

#[cfg(test)]
impl Capture {
    /// Everything written to the stdout sink so far.
    pub(crate) fn stdout(&self) -> String {
        self.out.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Everything written to the stderr sink so far.
    pub(crate) fn stderr(&self) -> String {
        self.err.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Both streams in the shape the snapshot files store.
    ///
    /// Stderr is included rather than dropped because *which* stream a message
    /// lands on is itself part of the contract: `print_error` writes to stderr
    /// in all three formats so that JSON mode leaves exactly one document on
    /// stdout.
    pub(crate) fn transcript(&self) -> String {
        let out = self.out.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let err = self.err.lock().unwrap_or_else(|e| e.into_inner()).clone();
        format!("--- stdout ---\n{out}--- stderr ---\n{err}")
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

/// The JSON body for a running daemon, shared by `daemon status` and
/// `stats --daemon`.
///
/// Both commands report the same daemon, but each used to hand-build its own
/// `json!` object — and they drifted: `stats --daemon` silently lacked the
/// heartbeat fields for as long as those existed.
///
/// `DaemonStatus` is flattened in rather than re-listed field by field, so a
/// field added to the wire struct reaches CLI output automatically. That works
/// because the wire struct is laid out in the shape the CLI wants: counters
/// grouped under `requests`, and `version` serialized as `protocol`. `running`
/// and `socket` are the only client-side facts left to add.
#[derive(Debug, serde::Serialize)]
pub struct DaemonStatusJson<'a> {
    /// Always true — this shape is only built for a daemon that answered.
    pub running: bool,
    pub socket: String,
    #[serde(flatten)]
    pub status: &'a engramdb::daemon::DaemonStatus,
}

impl<'a> DaemonStatusJson<'a> {
    pub fn new(status: &'a engramdb::daemon::DaemonStatus, socket: &std::path::Path) -> Self {
        Self {
            running: true,
            socket: socket.display().to_string(),
            status,
        }
    }
}

/// The JSON body for a running daemon, shared by both commands.
/// # Panics
///
/// Never in practice: [`DaemonStatusJson`] is a plain struct of strings,
/// integers, and `Option`/`Vec` of those, with a derived `Serialize` — none of
/// `to_value`'s failure modes (custom serializers, non-string map keys,
/// non-finite floats) can arise. Degrading to a partial `{"running": true}`
/// would be worse than failing loudly: scripted consumers would read `null`
/// for every field and could not distinguish that from a daemon reporting no
/// activity.
pub fn daemon_status_json(
    status: &engramdb::daemon::DaemonStatus,
    socket: &std::path::Path,
) -> serde_json::Value {
    serde_json::to_value(DaemonStatusJson::new(status, socket))
        .expect("DaemonStatusJson is plain data and cannot fail to serialize")
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
    use chrono::TimeZone as _;
    use engramdb::retrieval::engine::{RetrievalResult, ScoredMemory};
    use engramdb::scoring::ScoreBreakdown;
    use engramdb::types::{Memory, MemoryType, Provenance, Status, Visibility};

    /// A fixed instant for every fixture, so rendered timestamps are a
    /// constant rather than "whenever the suite ran".
    fn fixed(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(y, m, d, hh, mm, ss).unwrap()
    }

    /// Snapshot `rendered` under `name`.
    ///
    /// `snapshot_path` points out of `src/`: these assertions live next to the
    /// code they cover, but `.snap` files are test data and belong under
    /// `tests/`.
    ///
    /// No redaction is set up, and none is needed — every fixture here carries
    /// a pinned id and a pinned clock, which is the whole reason the format
    /// matrix is tested at this layer rather than through the binary.
    fn snap(name: &str, rendered: String) {
        insta::with_settings!({snapshot_path => "../tests/snapshots/renderer"}, {
            insta::assert_snapshot!(name, rendered);
        });
    }

    /// Render `body` in all three formats and snapshot each.
    fn snap_formats(case: &str, body: impl Fn(&OutputFormatter)) {
        for (format, suffix) in [
            (OutputFormat::Pretty, "pretty"),
            (OutputFormat::Json, "json"),
            (OutputFormat::Plain, "plain"),
        ] {
            let (formatter, cap) = OutputFormatter::capturing(format);
            body(&formatter);
            snap(&format!("{case}__{suffix}"), cap.transcript());
        }
    }

    /// Name the SGR parameter a colour escape carries.
    ///
    /// Only the codes this crate can emit are named; anything else keeps its
    /// number so an unexpected escape shows up in the snapshot instead of
    /// being flattened into a generic marker.
    fn sgr_name(param: &str) -> String {
        match param {
            "1" => "bold",
            "2" => "dim",
            "3" => "italic",
            "4" => "underline",
            "30" => "black",
            "31" => "red",
            "32" => "green",
            "33" => "yellow",
            "34" => "blue",
            "35" => "magenta",
            "36" => "cyan",
            "37" => "white",
            other => return format!("sgr{other}"),
        }
        .to_string()
    }

    /// Rewrite ANSI escapes into readable tags: `\x1b[32m✓\x1b[39m` becomes
    /// `<green>✓</green>`.
    ///
    /// Snapshots holding raw escape bytes are unreviewable — a diff on the web
    /// shows mojibake, and `cargo insta review` renders them as actual colour
    /// tangled up with insta's own diff colouring. Tags diff as text.
    ///
    /// This tracks the open styles rather than substituting literals, because
    /// the reset codes are shared: `\x1b[39m` closes whichever foreground
    /// colour is open, and `\x1b[0m` closes bold or dim. Carrying the stack is
    /// what lets a close tag name the style it closes.
    fn ansi_to_tags(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut open: Vec<String> = Vec::new();
        let mut rest = s;

        while let Some(start) = rest.find('\u{1b}') {
            out.push_str(&rest[..start]);
            let tail = &rest[start..];

            // Everything owo-colors writes is `ESC [ <params> m`.
            let parsed = tail
                .strip_prefix("\u{1b}[")
                .and_then(|b| b.find('m').map(|end| (&b[..end], &b[end + 1..])));
            let Some((params, after)) = parsed else {
                // Nothing here emits a non-SGR escape. Surface it rather than
                // letting a raw control byte through into a snapshot.
                out.push_str("<esc?>");
                rest = &tail['\u{1b}'.len_utf8()..];
                continue;
            };

            for param in params.split(';') {
                match param {
                    "0" | "22" | "39" | "49" => match open.pop() {
                        Some(name) => out.push_str(&format!("</{name}>")),
                        None => out.push_str("</?>"),
                    },
                    other => {
                        let name = sgr_name(other);
                        out.push_str(&format!("<{name}>"));
                        open.push(name);
                    }
                }
            }
            rest = after;
        }
        out.push_str(rest);

        assert!(
            open.is_empty(),
            "unclosed ANSI style(s) {open:?} — a renderer opened a style it never reset"
        );
        out
    }

    /// Render `body` with colour forced on and return the transcript.
    ///
    /// Two gates stand between a test runner and a styled render, and both
    /// have to be lifted: [`OutputFormatter::capturing_colored`] clears the
    /// formatter's own flag, and `owo_colors::with_override` short-circuits
    /// `if_supports_color`, which would otherwise ask `supports-color` about
    /// the real stdout and find a pipe.
    ///
    /// The override is a process-global `AtomicU8`, not thread-local. That is
    /// safe here only because nextest runs each test in its own process — the
    /// same property the `#[ctor]` env isolation depends on. `with_override`
    /// is scoped and RAII-restored regardless, including on panic.
    fn render_forcing_color(format: OutputFormat, body: impl Fn(&OutputFormatter)) -> String {
        let (formatter, cap) = OutputFormatter::capturing_colored(format);
        owo_colors::with_override(true, || body(&formatter));
        cap.transcript()
    }

    /// Snapshot a Pretty render with colour forced on, escapes rewritten as
    /// tags. Pretty is the only format [`OutputFormatter::styled`] admits, so
    /// there is no matrix here.
    ///
    /// The escapes-present assertion is load-bearing: without it, a broken
    /// override would leave every colour test passing on bare text, asserting
    /// nothing about colour at all.
    fn snap_colored(case: &str, body: impl Fn(&OutputFormatter)) {
        let raw = render_forcing_color(OutputFormat::Pretty, &body);
        assert!(
            raw.contains('\u{1b}'),
            "{case}: rendered no ANSI escapes with colour forced on"
        );
        snap(&format!("{case}__pretty_color"), ansi_to_tags(&raw));
    }

    /// The inverse claim, for the renderers that stay colourless on purpose:
    /// forcing colour on changes nothing.
    ///
    /// Asserting equality with the ordinary uncoloured render is stronger than
    /// a second snapshot would be — it pins the bytes to the ones
    /// [`snap_formats`] already reviewed, rather than to a near-duplicate copy
    /// of them — and it is a claim those snapshots structurally cannot make,
    /// since they render with the colour flag off and so look identical
    /// whether the renderer styles or not.
    fn assert_never_styled(case: &str, format: OutputFormat, body: impl Fn(&OutputFormatter)) {
        let forced = render_forcing_color(format, &body);
        assert!(
            !forced.contains('\u{1b}'),
            "{case}: expected no styling in {format:?}, got {forced:?}"
        );

        let (formatter, cap) = OutputFormatter::capturing(format);
        body(&formatter);
        assert_eq!(
            forced,
            cap.transcript(),
            "{case}: {format:?} output differs with colour forced on"
        );
    }

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
            audience: None,
            challenges: vec![],
            verified_at: None,
            created_at: fixed(2026, 1, 2, 3, 4, 5),
            updated_at: fixed(2026, 1, 2, 3, 4, 5),
            accessed_at: fixed(2026, 1, 2, 3, 4, 5),
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
            created_at: fixed(2026, 1, 2, 3, 4, 5),
            updated_at: fixed(2026, 1, 2, 3, 4, 5),
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

    /// Errors go to stderr in *every* format, so that JSON mode leaves exactly
    /// one document on stdout for a script to parse. Only a capturing sink can
    /// tell the two streams apart, which is why this was untestable before.
    #[test]
    fn errors_go_to_stderr_in_every_format() {
        for (format, expected) in [
            (OutputFormat::Json, "{\"error\":\"boom\"}\n"),
            (OutputFormat::Pretty, "✗ boom\n"),
            (OutputFormat::Plain, "Error: boom\n"),
        ] {
            let (formatter, cap) = OutputFormatter::capturing(format);
            formatter.print_error("boom");

            assert_eq!(cap.stderr(), expected, "stderr text for {format:?}");
            assert!(
                cap.stdout().is_empty(),
                "{format:?} put error text on stdout"
            );
            assert_eq!(
                cap.transcript(),
                format!("--- stdout ---\n--- stderr ---\n{expected}")
            );
        }
    }

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
        let (formatter, cap) = OutputFormatter::capturing(OutputFormat::Json);
        let results = vec![ScoredMemory {
            memory: test_memory(),
            score: 0.85,
            score_breakdown: test_score_breakdown(),
        }];

        formatter.print_search_results(&results);

        let parsed: serde_json::Value =
            serde_json::from_str(&cap.stdout()).expect("stdout must be one JSON document");
        assert_eq!(parsed[0]["score"], 0.85);
        assert_eq!(parsed[0]["memory"]["summary"], "Test summary");
        assert!(
            cap.stderr().is_empty(),
            "JSON mode must not write to stderr"
        );
    }

    #[test]
    fn test_search_results_empty() {
        for format in [
            OutputFormat::Json,
            OutputFormat::Pretty,
            OutputFormat::Plain,
        ] {
            let (formatter, cap) = OutputFormatter::capturing(format);
            formatter.print_search_results(&[]);

            let stdout = cap.stdout();
            match format {
                // An empty result set is still a valid document, not silence:
                // a script parsing stdout must get `[]`, not a parse error.
                OutputFormat::Json => assert_eq!(stdout.trim(), "[]"),
                _ => assert_eq!(stdout, "No memories found.\n"),
            }
        }
    }

    // ========================================
    // 4. print_retrieval_result format routing
    // ========================================

    /// `show_scores` is the only thing that gates the `breakdown` key, and it
    /// is the CLI's parity with the MCP `query` surface — so assert it both
    /// ways rather than just calling the method twice.
    #[test]
    fn test_retrieval_result_json_format() {
        let result = RetrievalResult {
            memories: vec![ScoredMemory {
                memory: test_memory(),
                score: 0.85,
                score_breakdown: test_score_breakdown(),
            }],
            total: 1,
            retrieval_quality: "full".to_string(),
        };

        let (formatter, cap) = OutputFormatter::capturing(OutputFormat::Json);
        formatter.print_retrieval_result(&result, true);
        let with: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert_eq!(with["total"], 1);
        assert_eq!(with["memories"][0]["breakdown"]["final_score"], 0.75);

        let (formatter, cap) = OutputFormatter::capturing(OutputFormat::Json);
        formatter.print_retrieval_result(&result, false);
        let without: serde_json::Value = serde_json::from_str(&cap.stdout()).unwrap();
        assert!(
            without["memories"][0].get("breakdown").is_none(),
            "breakdown must be absent without --show-scores"
        );
    }

    #[test]
    fn test_retrieval_result_empty() {
        let empty_result = RetrievalResult {
            memories: vec![],
            total: 0,
            retrieval_quality: "scope_only".to_string(),
        };

        for format in [
            OutputFormat::Json,
            OutputFormat::Pretty,
            OutputFormat::Plain,
        ] {
            let (formatter, cap) = OutputFormatter::capturing(format);
            formatter.print_retrieval_result(&empty_result, true);

            let stdout = cap.stdout();
            match format {
                OutputFormat::Json => {
                    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
                    assert_eq!(parsed["total"], 0);
                    assert_eq!(parsed["memories"].as_array().unwrap().len(), 0);
                }
                _ => assert_eq!(stdout, "No memories found.\n"),
            }
        }
    }

    // ========================================
    // 5. print_memory_list verbose flag
    // ========================================

    #[test]
    fn test_print_memory_list_json_ignores_verbose() {
        let entries = vec![test_index_entry()];

        let (formatter, verbose) = OutputFormatter::capturing(OutputFormat::Json);
        formatter.print_memory_list(&entries, true);
        let (formatter, terse) = OutputFormatter::capturing(OutputFormat::Json);
        formatter.print_memory_list(&entries, false);

        // The claim this test has always made, now actually checked: `verbose`
        // is a pretty/plain layout knob and must not reshape the JSON.
        assert_eq!(verbose.stdout(), terse.stdout());
    }

    #[test]
    fn test_print_memory_list_verbose_adds_detail_lines() {
        let entries = vec![test_index_entry()];

        let (formatter, terse) = OutputFormatter::capturing(OutputFormat::Pretty);
        formatter.print_memory_list(&entries, false);
        let (formatter, verbose) = OutputFormatter::capturing(OutputFormat::Pretty);
        formatter.print_memory_list(&entries, true);

        assert!(!terse.stdout().contains("Criticality"));
        assert!(verbose.stdout().contains("Criticality: 0.80"));
        assert!(verbose.stdout().starts_with(&terse.stdout()));
    }

    #[test]
    fn test_print_memory_list_empty() {
        for format in [
            OutputFormat::Json,
            OutputFormat::Pretty,
            OutputFormat::Plain,
        ] {
            let (formatter, cap) = OutputFormatter::capturing(format);
            formatter.print_memory_list(&[], true);

            let stdout = cap.stdout();
            match format {
                OutputFormat::Json => assert_eq!(stdout.trim(), "[]"),
                _ => assert_eq!(stdout, "No memories found.\n"),
            }
        }
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
            created_at: fixed(2026, 1, 2, 3, 4, 5),
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
            created_at: fixed(2026, 1, 2, 3, 4, 5),
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
            since: fixed(2026, 1, 1, 0, 0, 0),
            project_id: "project-a".to_string(),
            persistence_failures: 0,
            view,
            by_project: Some(by_project),
        }
    }

    fn empty_runtime_snapshot() -> engramdb::telemetry::RuntimeSnapshot {
        engramdb::telemetry::RuntimeSnapshot {
            since: fixed(2026, 1, 1, 0, 0, 0),
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

    /// `daemon status` and `stats --daemon` must emit the same schema. They
    /// used to build separate `json!` objects and drifted — `stats --daemon`
    /// was missing `ping_count` / `last_ping_secs_ago`. Both now go through
    /// [`daemon_status_json`], and this pins the keys so a future field is
    /// added in one place or not at all.
    #[test]
    fn daemon_status_json_has_stable_schema() {
        let status = engramdb::daemon::DaemonStatus {
            version: "4".to_string(),
            build: Some("0.9.0".to_string()),
            model_ids: vec!["embed=onnx/all-MiniLM-L12-v2-u8".to_string()],
            pid: 123,
            uptime_secs: 10,
            idle_secs: 1,
            bundles_loaded: 1,
            requests: engramdb::daemon::RequestCounts {
                embed: 1,
                classify: 2,
                rerank: 3,
                meta: 4,
                status: 5,
                title: 6,
                total: 21,
            },
            ping_count: 7,
            last_ping_secs_ago: Some(8),
        };
        let value = daemon_status_json(&status, std::path::Path::new("/tmp/d.sock"));
        let obj = value.as_object().expect("object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "build",
                "bundles_loaded",
                "idle_secs",
                "last_ping_secs_ago",
                "model_ids",
                "pid",
                "ping_count",
                "protocol",
                "requests",
                "running",
                "socket",
                "uptime_secs",
            ]
        );

        // `version` is presented as `protocol`; the raw wire spelling and the
        // flat counters must not leak through.
        assert_eq!(obj["protocol"], "4");
        assert!(!obj.contains_key("version"));
        assert!(!obj.contains_key("requests_embed"));
        assert_eq!(obj["requests"]["total"], 21);
        assert_eq!(obj["model_ids"][0], "embed=onnx/all-MiniLM-L12-v2-u8");
    }

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

    // =====================================================================
    // 10. The format matrix — every public renderer × pretty/json/plain
    //
    // These are the snapshots that make a layout change visible. They are
    // taken here rather than through the binary because every input is a
    // literal: pinned ids, pinned clocks, no store, no models, no temp paths.
    // That means the snapshots hold the real rendered bytes with nothing
    // redacted, so a reviewer reads the actual output instead of a field of
    // placeholders.
    //
    // The binary-level counterparts live in `tests/cli/snapshot/`, which
    // covers what this layer structurally cannot: exit codes, stream routing
    // through a real pipe, and clap's own errors.
    // =====================================================================

    /// A memory with every optional field populated.
    ///
    /// The plain/pretty renderers skip absent fields, so [`test_memory`]
    /// alone exercises only the "everything is None" layout — this is the
    /// other end of that.
    fn rich_memory() -> Memory {
        use engramdb::types::{Decay, DecayStrategy, Epistemic, Generality, Validity};

        Memory {
            id: "018f2a1b-3c4d-7e5f-8a9b-0c1d2e3f4a5b".to_string(),
            type_: MemoryType::Hazard,
            // Off-diagonal from the type default, so `epistemic_tags` renders
            // a class tag.
            epistemic: Epistemic::Observation,
            valid_while: Some(Validity {
                premise: Some("while ort is pinned to rc.12".to_string()),
                invalidated_by: vec!["Cargo.lock".to_string(), "build.rs".to_string()],
                origin_task: Some("daemon-perf".to_string()),
                generality: Generality::Task,
                derived_from: vec!["550e8400-e29b-41d4-a716-446655440000".to_string()],
            }),
            valid_from: Some(fixed(2025, 12, 1, 0, 0, 0)),
            // Well in the past, so the tag is a tombstone (`invalidated`)
            // rather than a schedule, no matter when the suite runs.
            invalidated_at: Some(fixed(2020, 6, 15, 12, 0, 0)),
            superseded_by: Some("018f2a1b-3c4d-7e5f-8a9b-000000000099".to_string()),
            summary: "Blocking calls deadlock the daemon".to_string(),
            title: Some("daemon-deadlock".to_string()),
            content: "A blocking call on the async runtime stalls every session.".to_string(),
            details: Some("Reproduced twice under load.".to_string()),
            physical: vec![
                "src/daemon/server.rs".to_string(),
                "src/daemon/*.rs".to_string(),
            ],
            logical: vec!["daemon.runtime".to_string(), "perf".to_string()],
            tags: vec!["async".to_string(), "deadlock".to_string()],
            criticality: 0.95,
            decay: Some(Decay {
                strategy: DecayStrategy::Exponential,
                half_life: Some(chrono::TimeDelta::seconds(86_400)),
                ttl: Some(chrono::TimeDelta::seconds(604_800)),
                floor: 0.1,
            }),
            provenance: Provenance::agent("claude"),
            confidence: 0.65,
            supersedes: vec!["018f2a1b-3c4d-7e5f-8a9b-000000000011".to_string()],
            status: Status::Active,
            visibility: Visibility::Personal,
            audience: Some(vec!["__g_abcdef012345".to_string()]),
            challenges: vec![],
            verified_at: Some(fixed(2026, 2, 3, 4, 5, 6)),
            created_at: fixed(2026, 1, 2, 3, 4, 5),
            updated_at: fixed(2026, 1, 3, 4, 5, 6),
            accessed_at: fixed(2026, 1, 4, 5, 6, 7),
            expires_at: Some(fixed(2027, 1, 1, 0, 0, 0)),
        }
    }

    fn project_info() -> ProjectInfoOutput {
        ProjectInfoOutput {
            project_id: "0123456789abcdef".to_string(),
            project_name: "engramdb".to_string(),
            project_path: "/w/engramdb".to_string(),
            memory_count: 42,
            logical_scopes: vec!["daemon".to_string(), "retrieval".to_string()],
            created_at: fixed(2026, 1, 2, 3, 4, 5),
            parent_project_id: None,
        }
    }

    // ---- messages -------------------------------------------------------

    #[test]
    fn snap_print_message() {
        snap_formats("message", |f| f.print_message("a plain message"));
    }

    #[test]
    fn snap_print_success() {
        snap_formats("success", |f| f.print_success("it worked"));
    }

    #[test]
    fn snap_print_error() {
        snap_formats("error", |f| f.print_error("it did not work"));
    }

    #[test]
    fn snap_print_warning() {
        snap_formats("warning", |f| f.print_warning("proceed with care"));
    }

    /// Hints are suppressed in JSON (they would corrupt the single document),
    /// which is exactly the kind of per-format divergence worth pinning.
    #[test]
    fn snap_print_hint() {
        snap_formats("hint", |f| f.print_hint("try --force"));
    }

    // ---- a single memory ------------------------------------------------

    #[test]
    fn snap_memory_minimal() {
        snap_formats("memory_minimal", |f| f.print_memory(&test_memory()));
    }

    #[test]
    fn snap_memory_rich() {
        snap_formats("memory_rich", |f| f.print_memory(&rich_memory()));
    }

    #[test]
    fn snap_memory_full() {
        snap_formats("memory_full", |f| f.print_memory_full(&rich_memory()));
    }

    // ---- search / retrieval ---------------------------------------------

    #[test]
    fn snap_search_results() {
        let results = vec![
            ScoredMemory {
                memory: test_memory(),
                score: 0.85,
                score_breakdown: test_score_breakdown(),
            },
            ScoredMemory {
                memory: rich_memory(),
                score: 0.42,
                score_breakdown: test_score_breakdown(),
            },
        ];
        snap_formats("search_results", |f| f.print_search_results(&results));
    }

    #[test]
    fn snap_search_results_empty() {
        snap_formats("search_results_empty", |f| f.print_search_results(&[]));
    }

    fn retrieval_result() -> RetrievalResult {
        RetrievalResult {
            memories: vec![
                ScoredMemory {
                    memory: test_memory(),
                    score: 0.85,
                    score_breakdown: test_score_breakdown(),
                },
                ScoredMemory {
                    memory: rich_memory(),
                    score: 0.42,
                    score_breakdown: test_score_breakdown(),
                },
            ],
            total: 7,
            retrieval_quality: "full".to_string(),
        }
    }

    #[test]
    fn snap_retrieval_result_with_scores() {
        snap_formats("retrieval_with_scores", |f| {
            f.print_retrieval_result(&retrieval_result(), true)
        });
    }

    #[test]
    fn snap_retrieval_result_without_scores() {
        snap_formats("retrieval_without_scores", |f| {
            f.print_retrieval_result(&retrieval_result(), false)
        });
    }

    #[test]
    fn snap_retrieval_result_empty() {
        let empty = RetrievalResult {
            memories: vec![],
            total: 0,
            retrieval_quality: "scope_only".to_string(),
        };
        snap_formats("retrieval_empty", |f| {
            f.print_retrieval_result(&empty, true)
        });
    }

    // ---- list -----------------------------------------------------------

    fn index_entries() -> Vec<IndexFilterable> {
        let mut second = test_index_entry();
        second.id = "018f2a1b-3c4d-7e5f-8a9b-0c1d2e3f4a5b".to_string();
        second.type_ = MemoryType::Hazard;
        second.epistemic = engramdb::types::Epistemic::Observation;
        second.summary = "Blocking calls deadlock the daemon".to_string();
        second.tags = vec!["async".to_string()];
        second.criticality = 0.95;
        second.invalidated_at = Some(fixed(2020, 6, 15, 12, 0, 0));
        vec![test_index_entry(), second]
    }

    #[test]
    fn snap_memory_list() {
        snap_formats("memory_list", |f| {
            f.print_memory_list(&index_entries(), false)
        });
    }

    #[test]
    fn snap_memory_list_verbose() {
        snap_formats("memory_list_verbose", |f| {
            f.print_memory_list(&index_entries(), true)
        });
    }

    #[test]
    fn snap_memory_list_empty() {
        snap_formats("memory_list_empty", |f| f.print_memory_list(&[], true));
    }

    // ---- stats ----------------------------------------------------------

    #[test]
    fn snap_stats_without_runtime() {
        let stats = Stats {
            total: 3,
            by_type: vec![(MemoryType::Decision, 2), (MemoryType::Convention, 1)],
            by_status: vec![(Status::Active, 3)],
            by_scope: vec![("services/api".to_string(), 2)],
            expired: 1,
            oldest: Some(fixed(2025, 1, 1, 0, 0, 0)),
            newest: Some(fixed(2026, 1, 1, 0, 0, 0)),
            avg_criticality: 0.75,
            runtime: None,
        };
        snap_formats("stats_plain_counts", |f| f.print_stats(&stats));
    }

    #[test]
    fn snap_stats_with_full_runtime() {
        snap_formats("stats_with_runtime", |f| {
            f.print_stats(&stats_with_runtime(fully_populated_runtime_snapshot()))
        });
    }

    #[test]
    fn snap_stats_with_empty_runtime() {
        snap_formats("stats_with_empty_runtime", |f| {
            f.print_stats(&stats_with_runtime(empty_runtime_snapshot()))
        });
    }

    // ---- projects -------------------------------------------------------

    #[test]
    fn snap_project_info() {
        snap_formats("project_info", |f| f.print_project_info(&project_info()));
    }

    #[test]
    fn snap_project_info_with_parent() {
        let mut info = project_info();
        info.parent_project_id = Some("fedcba9876543210".to_string());
        snap_formats("project_info_with_parent", |f| f.print_project_info(&info));
    }

    fn project_entries() -> Vec<ProjectListOutput> {
        vec![
            ProjectListOutput {
                project_id: "0123456789abcdef".to_string(),
                project_path: "/w/engramdb".to_string(),
                exists: true,
                parent_project_id: None,
            },
            // A worktree child: nests under its parent with a `↳` marker.
            ProjectListOutput {
                project_id: "fedcba9876543210".to_string(),
                project_path: "/w/engramdb-wt".to_string(),
                exists: true,
                parent_project_id: Some("0123456789abcdef".to_string()),
            },
            // A registered path that is gone: renders the `missing` status.
            ProjectListOutput {
                project_id: "aaaabbbbccccdddd".to_string(),
                project_path: "/w/deleted".to_string(),
                exists: false,
                parent_project_id: None,
            },
        ]
    }

    /// Grouping is a layout decision with three distinct shapes — a header
    /// above every directory, headers only where a directory holds more than
    /// one project, and no headers at all — so each gets its own snapshot.
    #[test]
    fn snap_project_list_grouping_always() {
        snap_formats("project_list_always", |f| {
            f.print_project_list(&project_entries(), ProjectListGrouping::Always)
        });
    }

    #[test]
    fn snap_project_list_grouping_auto() {
        snap_formats("project_list_auto", |f| {
            f.print_project_list(&project_entries(), ProjectListGrouping::Auto)
        });
    }

    #[test]
    fn snap_project_list_grouping_none() {
        snap_formats("project_list_none", |f| {
            f.print_project_list(&project_entries(), ProjectListGrouping::None)
        });
    }

    #[test]
    fn snap_project_list_empty() {
        snap_formats("project_list_empty", |f| {
            f.print_project_list(&[], ProjectListGrouping::Auto)
        });
    }

    #[test]
    fn snap_aggregate_stats() {
        let stats = AggregateStatsOutput {
            total_projects: 3,
            reachable_projects: 2,
            total_memories: 128,
            by_type: vec![(MemoryType::Decision, 80), (MemoryType::Hazard, 48)],
        };
        snap_formats("aggregate_stats", |f| f.print_aggregate_stats(&stats));
    }

    // ---- doctor ---------------------------------------------------------

    #[test]
    fn snap_environment_doctor() {
        snap_formats("environment_doctor", |f| {
            f.print_environment_doctor(&doctor_result_with_all_statuses())
        });
    }

    #[test]
    fn snap_environment_doctor_minimal() {
        snap_formats("environment_doctor_minimal", |f| {
            f.print_environment_doctor(&test_environment_doctor_result())
        });
    }

    // =====================================================================
    // 11. The colour matrix
    //
    // Pretty is the only format that styles, so these are single-format
    // rather than a three-way sweep — the json/plain bytes are already
    // pinned above and re-taking them under a colour override would assert
    // the same thing twice. Each case reuses the fixture its uncoloured twin
    // uses, so `<case>__pretty.snap` and `<case>__pretty_color.snap` sit side
    // by side and diff as "same layout, plus styling".
    //
    // This is a tier-1-only concern. `OutputFormatter::new` checks `is_tty`
    // itself, before owo-colors is consulted, so no environment variable can
    // make the real binary emit colour into a pipe — a tier-2 colour case
    // would need a PTY harness to re-test rendering that is entirely in this
    // file. The tier-2 direction that *is* worth having is the negative one,
    // and its snapshots already carry it: any escape leaking to a pipe would
    // show up there.
    // =====================================================================

    #[test]
    fn ansi_to_tags_names_the_style_it_closes() {
        // The two reset codes in play: `39` ends a foreground colour, `0`
        // ends bold/dim. Both have to resolve to the style they close.
        assert_eq!(ansi_to_tags("\u{1b}[32mx\u{1b}[39m"), "<green>x</green>");
        assert_eq!(ansi_to_tags("\u{1b}[1mh\u{1b}[0m"), "<bold>h</bold>");
        assert_eq!(ansi_to_tags("\u{1b}[2md\u{1b}[0m"), "<dim>d</dim>");
        // Two independently styled spans on one line — the shape almost every
        // renderer here produces.
        assert_eq!(
            ansi_to_tags("\u{1b}[36mid\u{1b}[39m \u{1b}[33mDecision\u{1b}[39m"),
            "<cyan>id</cyan> <yellow>Decision</yellow>"
        );
        assert_eq!(ansi_to_tags("no escapes here"), "no escapes here");
        // An unrecognized code keeps its number rather than vanishing.
        assert_eq!(ansi_to_tags("\u{1b}[7mv\u{1b}[0m"), "<sgr7>v</sgr7>");
    }

    // ---- messages -------------------------------------------------------

    #[test]
    fn snap_color_success() {
        snap_colored("success", |f| f.print_success("it worked"));
    }

    /// Errors and warnings style *stderr*, so the escapes have to show up on
    /// the second half of the transcript.
    #[test]
    fn snap_color_error() {
        snap_colored("error", |f| f.print_error("it did not work"));
    }

    #[test]
    fn snap_color_warning() {
        snap_colored("warning", |f| f.print_warning("proceed with care"));
    }

    #[test]
    fn snap_color_hint() {
        snap_colored("hint", |f| f.print_hint("try --force"));
    }

    /// `print_message` is the one message renderer with no styled branch.
    #[test]
    fn snap_color_message_stays_plain() {
        assert_never_styled("message", OutputFormat::Pretty, |f| {
            f.print_message("a plain message")
        });
    }

    // ---- a single memory ------------------------------------------------

    #[test]
    fn snap_color_memory_rich() {
        snap_colored("memory_rich", |f| f.print_memory(&rich_memory()));
    }

    // ---- search / retrieval ---------------------------------------------

    #[test]
    fn snap_color_search_results() {
        let results = vec![
            ScoredMemory {
                memory: test_memory(),
                score: 0.85,
                score_breakdown: test_score_breakdown(),
            },
            ScoredMemory {
                memory: rich_memory(),
                score: 0.42,
                score_breakdown: test_score_breakdown(),
            },
        ];
        snap_colored("search_results", |f| f.print_search_results(&results));
    }

    /// `--show-scores` picks a different styled branch, not just an extra
    /// column, so both are taken.
    #[test]
    fn snap_color_retrieval_with_scores() {
        snap_colored("retrieval_with_scores", |f| {
            f.print_retrieval_result(&retrieval_result(), true)
        });
    }

    #[test]
    fn snap_color_retrieval_without_scores() {
        snap_colored("retrieval_without_scores", |f| {
            f.print_retrieval_result(&retrieval_result(), false)
        });
    }

    // ---- list -----------------------------------------------------------

    #[test]
    fn snap_color_memory_list() {
        snap_colored("memory_list", |f| {
            f.print_memory_list(&index_entries(), false)
        });
    }

    /// The verbose detail lines are colourless even in Pretty; the entry
    /// lines above them are not. Pinning the verbose case keeps that split
    /// visible.
    #[test]
    fn snap_color_memory_list_verbose() {
        snap_colored("memory_list_verbose", |f| {
            f.print_memory_list(&index_entries(), true)
        });
    }

    // ---- stats ----------------------------------------------------------

    /// Stats renders no colour at all, in either the counters or the runtime
    /// block — the densest colourless renderer, and the easiest one to start
    /// styling by accident.
    #[test]
    fn snap_color_stats_stays_plain() {
        assert_never_styled("stats_with_runtime", OutputFormat::Pretty, |f| {
            f.print_stats(&stats_with_runtime(fully_populated_runtime_snapshot()))
        });
    }

    // ---- projects -------------------------------------------------------

    #[test]
    fn snap_color_project_info() {
        snap_colored("project_info", |f| f.print_project_info(&project_info()));
    }

    #[test]
    fn snap_color_project_info_with_parent() {
        let mut info = project_info();
        info.parent_project_id = Some("fedcba9876543210".to_string());
        snap_colored("project_info_with_parent", |f| f.print_project_info(&info));
    }

    /// `Always` grouping is the case that renders every styled element the
    /// project tree has: a dimmed directory header, cyan ids, and the red
    /// `missing` status on the entry whose path is gone.
    #[test]
    fn snap_color_project_list() {
        snap_colored("project_list_always", |f| {
            f.print_project_list(&project_entries(), ProjectListGrouping::Always)
        });
    }

    /// The regression this pair exists for: `print_project_list` runs Pretty
    /// and Plain through one `render_project_line`, which used to consult
    /// `use_color` alone — so `--format plain` on a terminal came out
    /// coloured, against the contract at the top of this module. Plain must
    /// produce the same tree with no styling.
    #[test]
    fn snap_color_project_list_plain_is_never_styled() {
        assert_never_styled("project_list_always", OutputFormat::Plain, |f| {
            f.print_project_list(&project_entries(), ProjectListGrouping::Always)
        });
    }

    // ---- doctor ---------------------------------------------------------

    /// The richest styled renderer: bold section headers, a per-status icon
    /// colour, dimmed info rows and detail lines, and blue hints — in both
    /// the section and subsection loops, which style independently.
    #[test]
    fn snap_color_environment_doctor() {
        snap_colored("environment_doctor", |f| {
            f.print_environment_doctor(&doctor_result_with_all_statuses())
        });
    }
}
