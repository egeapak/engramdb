//! Claude Code transcript discovery and parsing.
//!
//! Claude Code writes one append-only JSONL file per session:
//!
//! ```text
//! ~/.claude/projects/<encoded-cwd>/<session-id>.jsonl
//! ```
//!
//! where `<encoded-cwd>` is the session's working directory with every
//! non-alphanumeric byte replaced by `-` ([`encode_project_dir`]). That
//! encoding is **lossy** — `/a/b.c` and `/a/b-c` collide, and so do a repo
//! and its dotfile sibling — so it is only ever used as a *fast path* here.
//! Every record additionally carries the authoritative `cwd` it was written
//! from, and [`list_sessions_for`] verifies against that field before
//! attributing a transcript to a project. A wrong attribution would feed one
//! project's conversations into another project's memory store, so the
//! recorded `cwd` always wins over the directory name.
//!
//! ## Why parsing lives here rather than in `ops`
//!
//! Transcripts are read-only foreign state on disk, in the same category as
//! the registry and the session→task mapping: locating and decoding them is
//! storage's job. This module deliberately stops at a normalized
//! [`Event`] stream — budgeting, redaction, and rendering are policy and
//! live in `ops::harvest`, which may depend on this module but never the
//! reverse.
//!
//! ## Signal density
//!
//! A transcript is dominated by tool traffic: measured over a real session,
//! `tool_result` blocks plus attachments accounted for ~37% of the bytes and
//! tool-call arguments another ~4%, while the *prose* that actually carries
//! durable knowledge — user prompts and assistant replies — came to under
//! 1%. Feeding raw JSONL to an agent therefore spends the whole context
//! window on noise. [`parse_session`] keeps prompts and replies verbatim,
//! reduces every tool call to a name plus a short target, and keeps only a
//! first-line preview of each result — with the **error** flag preserved,
//! because "this command failed and that one worked" is frequently the
//! durable lesson and it only exists in the result.

use crate::error::{Result, StorageError};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

/// Preview length for the first user prompt shown in a session listing.
const PROMPT_PREVIEW_CHARS: usize = 160;

/// Preview length kept from a tool result. Enough for a one-line error or a
/// short success line; anything longer is noise for harvesting purposes.
const RESULT_PREVIEW_CHARS: usize = 200;

/// Largest single JSONL record this parser will hold in memory.
///
/// Measured over the real transcripts on a development machine (3,036
/// records): p50 1.5 KB, p99 33 KB, largest 95 KB — but that corpus contained
/// no **pasted images**, which Claude Code embeds as base64 inside the record
/// and which inflate 4/3 over the original file. A pasted screenshot is
/// therefore the one ordinary record class that can run to megabytes, and
/// dropping it would silently lose the human turn attached to it (and shift
/// `first_prompt` to a different turn). 4 MiB clears a realistic screenshot
/// while still bounding what one hostile line can cost.
///
/// Public because a dropped record is a loss the digest has to declare, and
/// declaring it means naming the ceiling it hit.
pub const MAX_RECORD_BYTES: usize = 4 * 1_048_576;

/// Root of Claude Code's own state directory.
///
/// Honors `CLAUDE_CONFIG_DIR` (Claude Code's documented override) so a test
/// or a non-default install still resolves correctly, then falls back to
/// `~/.claude`.
pub fn claude_home() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("CLAUDE_CONFIG_DIR") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    dirs::home_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine home directory".to_string()))
        .map(|p| p.join(".claude"))
}

/// Directory holding one subdirectory of transcripts per project.
pub fn projects_root() -> Result<PathBuf> {
    Ok(claude_home()?.join("projects"))
}

/// Encode a working directory the way Claude Code names its transcript
/// directory: every character outside `[A-Za-z0-9]` becomes `-`.
///
/// One dash per Unicode scalar, not per UTF-8 byte. Claude Code does this in
/// JS (`replace(/[^a-zA-Z0-9]/g, '-')`, which iterates UTF-16 code units),
/// so the two agree for every BMP character and disagree for astral ones —
/// it would emit two dashes for an emoji where this emits one. That is not
/// observable from here, so `list_sessions_in` stands the encoded-name fast
/// path down entirely for non-ASCII paths rather than trusting a guess.
///
/// Lossy by construction — see the module docs. Callers must treat a match
/// as a candidate to verify, not as proof.
pub fn encode_project_dir(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Is this string safe to use as a single filesystem path component?
///
/// Session ids arrive from outside the process — hook event JSON on stdin and
/// MCP tool arguments — and are then joined into archive paths and used as
/// ledger keys. `Path::join` resolves `..` and lets an *absolute* string
/// replace the base entirely, so an unchecked id is an arbitrary-file
/// read/write/delete primitive rather than a naming inconvenience.
///
/// Claude Code emits UUIDs, so the accepted set is deliberately narrower than
/// "no separators": alphanumerics, `-`, `_`, and `.` — with any `..` run and
/// a leading `.` rejected so the result can never traverse or hide.
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Invisible-format test shared with [`sanitize_for_terminal`], for callers
/// that must match against raw (un-sanitized) record text.
pub fn is_invisible_format(c: char) -> bool {
    // Unicode's `Default_Ignorable_Code_Point` set, *minus* the bidi controls
    // (which `is_unsafe` marks with U+FFFD instead, because reordered text is
    // a forgery worth showing), *plus* a few characters that are not formally
    // ignorable but still render as blank.
    //
    // Defined by the Unicode property rather than by hand-picking characters
    // seen in an attack: the first version of this list was assembled from
    // observed payloads, and a review promptly found sixteen more. A
    // property-defined set has an end; "characters someone tried" does not.
    matches!(c,
        '\u{00ad}'                  // soft hyphen
        | '\u{034f}'                // combining grapheme joiner
        | '\u{115f}'..='\u{1160}'   // Hangul choseong/jungseong fillers
        | '\u{17b4}'..='\u{17b5}'   // Khmer inherent vowels
        | '\u{180b}'..='\u{180f}'   // Mongolian FVS 1-4 + vowel separator
        | '\u{200b}'..='\u{200d}'   // ZWSP, ZWNJ, ZWJ
        | '\u{2060}'..='\u{206f}'   // word joiner, invisible ops, deprecated
        | '\u{2800}'                // braille pattern blank
        | '\u{3164}'                // Hangul filler
        | '\u{fe00}'..='\u{fe0f}'   // variation selectors
        | '\u{feff}'                // BOM / ZWNBSP
        | '\u{ffa0}'                // halfwidth Hangul filler
        | '\u{fff0}'..='\u{fff8}'   // unassigned, formally ignorable
        | '\u{0600}'..='\u{0605}'   // Arabic number signs
        | '\u{06dd}'                // Arabic end of ayah
        | '\u{070f}'                // Syriac abbreviation mark
        | '\u{08e2}'                // Arabic disputed end of ayah
        | '\u{0890}'..='\u{0891}'   // Arabic pound/piastre mark above
        | '\u{110bd}'               // Kaithi number sign
        | '\u{110cd}'               // Kaithi number sign above
        | '\u{13430}'..='\u{1343f}' // Egyptian format controls
        | '\u{1bca0}'..='\u{1bca3}' // shorthand format controls
        | '\u{1d173}'..='\u{1d17a}' // musical formatting
        | '\u{e0000}'..='\u{e0fff}' // tags + variation selectors supplement
    )
}

/// Neutralize bytes a terminal would *execute*, or a matcher would miss.
///
/// Transcript text was written by another program and fed by arbitrary third
/// parties — web pages, PR comments, dependency source. Rendering it to a
/// human's screen is the one path where those bytes reach a terminal rather
/// than a model's context, and an escape sequence there can repaint the line,
/// hide a command, emit a clickable hyperlink, or write the clipboard.
///
/// Two dispositions, for two different problems: characters that *do*
/// something become `U+FFFD` (a visible mark — silently stripping them would
/// make tampered content look clean), while characters that render as
/// *nothing* are deleted outright, so the matchers downstream see the string a
/// reader sees. Returns `Cow::Borrowed` for clean input, which is nearly all
/// of it, so calling this per row costs no allocation.
pub fn sanitize_for_terminal(text: &str) -> std::borrow::Cow<'_, str> {
    /// Characters that render as nothing at all.
    ///
    /// These are *deleted*, not replaced. Replacing would be the safer-looking
    /// choice but is exactly wrong here: the whole attack is that
    /// `<system\u{200d}-reminder>` and `\u{200d}### Human` defeat a literal
    /// matcher while looking identical to the real thing on screen. Deleting
    /// reassembles the string the matchers downstream actually need to see,
    /// and nothing visible is lost — that is what "invisible" means.
    ///
    /// The cost is that a ZWJ emoji sequence decomposes (a family emoji
    /// renders as its component people) and ZWNJ-dependent shaping in Persian
    /// and some Indic scripts is lost. For a transcript digest mined for
    /// facts, that is a fair trade against an undetectable forgery.
    fn is_invisible(c: char) -> bool {
        // Same set as the free function above; kept as a local alias so the
        // hot loop does not pay a call through a pub boundary.
        is_invisible_format(c)
    }

    /// Characters replaced with U+FFFD: they do something a reader would not
    /// sanction, and leaving a visible mark is the point — silently stripping
    /// them would make tampered content indistinguishable from clean content.
    fn is_unsafe(c: char) -> bool {
        match c {
            // Newline and tab are legitimate structure in a digest body.
            '\n' | '\t' => false,
            // C0 (incl. ESC and BEL) and DEL.
            c if (c as u32) < 0x20 || c == '\u{7f}' => true,
            // C1: U+009B is CSI and U+009D is OSC on many terminals.
            c if ('\u{80}'..='\u{9f}').contains(&c) => true,
            // Bidi overrides and isolates — Trojan Source: a preview can be
            // made to render in an order that inverts what it says. Marked
            // rather than deleted, because reordered text *is* a forgery.
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
            '\u{200e}' | '\u{200f}' | '\u{061c}' => true,
            // Unicode line/paragraph separators. Callers split on `\n`, so a
            // segment after one of these is never probed by the structural
            // escape — it would be an unexamined line by construction.
            '\u{2028}' | '\u{2029}' => true,
            _ => false,
        }
    }

    if !text.chars().any(|c| is_unsafe(c) || is_invisible(c)) && !text.contains('\r') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if is_invisible(c) {
            continue;
        }
        if c == '\r' {
            // CRLF is an ordinary line ending; a lone CR rewrites the line
            // that was already printed.
            if chars.peek() == Some(&'\n') {
                continue;
            }
            out.push('\u{fffd}');
        } else if is_unsafe(c) {
            out.push('\u{fffd}');
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// [`sanitize_for_terminal`], then flatten to a single line.
///
/// For values the caller renders *inside* one structural line — a branch
/// name, a one-line preview — where a raw newline would forge an entire
/// extra row of output.
pub fn sanitize_one_line(text: &str) -> std::borrow::Cow<'_, str> {
    let cleaned = sanitize_for_terminal(text);
    if !cleaned.contains(['\n', '\t']) {
        return cleaned;
    }
    let flattened = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    std::borrow::Cow::Owned(flattened)
}

/// Metadata for one session transcript, cheap enough to compute for every
/// session in a project before deciding which ones to digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session id (the transcript's file stem).
    pub session_id: String,
    /// Absolute path of the `.jsonl` transcript.
    pub transcript_path: PathBuf,
    /// Working directory recorded *inside* the transcript. Authoritative for
    /// project attribution; `None` when no record carried one.
    pub cwd: Option<String>,
    /// Git branch recorded in the transcript, when present.
    pub git_branch: Option<String>,
    /// Timestamp of the first and last timestamped record.
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Human turns (user messages that are real prompts, not tool results).
    pub user_turns: usize,
    /// Assistant messages carrying prose.
    pub assistant_turns: usize,
    /// Transcript size on disk, in bytes.
    pub bytes: u64,
    /// Truncated preview of the first human prompt — the single most useful
    /// field for deciding whether a session is worth digesting.
    pub first_prompt: Option<String>,
    /// Records dropped whole for exceeding [`MAX_RECORD_BYTES`].
    ///
    /// Every other count here is then a *lower bound*, and `first_prompt` may
    /// belong to a later turn than the one the human actually opened with. The
    /// loss is unrecoverable at this layer — the bytes were never parsed — so
    /// it has to travel with the summary rather than being logged and
    /// forgotten. `serde(default)` because summaries serialized before this
    /// field existed are still read back.
    #[serde(default)]
    pub skipped_records: usize,
}

/// One normalized, harvest-relevant event from a transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A human prompt.
    UserPrompt {
        at: Option<DateTime<Utc>>,
        text: String,
    },
    /// Assistant prose (the `text` blocks, not tool calls).
    AssistantText {
        at: Option<DateTime<Utc>>,
        text: String,
    },
    /// Assistant reasoning. Off by default: verbose, and its conclusions
    /// almost always resurface in the prose or the actions that follow.
    Thinking {
        at: Option<DateTime<Utc>>,
        text: String,
    },
    /// A tool invocation reduced to name + target, with the outcome of its
    /// result if one was found.
    ToolCall {
        at: Option<DateTime<Utc>>,
        name: String,
        /// Short human-readable target (file path, command, pattern, …).
        target: Option<String>,
        /// `Some(false)` when the result was flagged as an error, `Some(true)`
        /// on success, `None` when no matching result was found (the session
        /// ended, or the call was rejected).
        ok: Option<bool>,
        /// First-line preview of the result.
        result_preview: Option<String>,
    },
}

impl Event {
    /// Timestamp, when the source record carried one.
    pub fn at(&self) -> Option<DateTime<Utc>> {
        match self {
            Event::UserPrompt { at, .. }
            | Event::AssistantText { at, .. }
            | Event::Thinking { at, .. }
            | Event::ToolCall { at, .. } => *at,
        }
    }
}

/// How much of the tool trace a consumer wants.
///
/// A three-state axis rather than the `bool` it replaced, because the search
/// index needs a third disposition the bool could not express: drop the tool
/// noise that dilutes an embedding, but keep every *failure* and the error
/// text behind it. "This command failed and that one worked" is frequently the
/// durable lesson, and it is more true of search than of an agent's read —
/// "why did the build break in July" is a question about a failure.
///
/// Two booleans would encode the same three states plus an illegal fourth, so
/// this is an enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDetail {
    /// Every tool call, successful or not.
    All,
    /// Only calls whose result came back an error. A call whose result never
    /// arrived (`ok == None` — the session was cut off mid-call) is *not* a
    /// known failure and is dropped with the rest.
    FailuresOnly,
    /// No tool calls at all: prompts and prose only.
    None,
}

/// What to keep when parsing a transcript.
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    /// Include assistant reasoning blocks. Default `false`.
    pub include_thinking: bool,
    /// Include subagent (`isSidechain`) turns. Default `false`: subagents
    /// report their findings back into the main thread, so their raw turns
    /// are mostly duplicate volume.
    pub include_sidechains: bool,
    /// Which tool calls survive. Default [`ToolDetail::All`] — the sequence of
    /// actions is often where a convention or hazard is visible.
    pub tools: ToolDetail,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_sidechains: false,
            tools: ToolDetail::All,
        }
    }
}

/// A parsed transcript: metadata plus its normalized event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedSession {
    pub summary: SessionSummary,
    pub events: Vec<Event>,
}

/// Truncate on a char boundary, appending an ellipsis when shortened.
fn truncate_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Collapse a value to a single-line preview.
fn first_line_preview(s: &str, max: usize) -> Option<String> {
    let line = s.lines().find(|l| !l.trim().is_empty())?;
    Some(truncate_chars(line, max))
}

/// Derive a short, human-meaningful target from a tool call's arguments.
///
/// The keys are checked in descending order of specificity; unknown tools
/// fall back to `None` rather than dumping an arbitrary JSON blob, which
/// would reintroduce exactly the payload volume this module exists to strip.
fn tool_target(input: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 7] = [
        "file_path",
        "command",
        "pattern",
        "path",
        "url",
        "notebook_path",
        "description",
    ];
    for key in KEYS {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(truncate_chars(v, 120));
            }
        }
    }
    None
}

/// Extract the text of a `tool_result` block, whose `content` is either a
/// bare string or an array of typed blocks.
fn tool_result_text(block: &serde_json::Value) -> Option<String> {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(items)) => {
            let joined: Vec<&str> = items
                .iter()
                .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                .collect();
            if joined.is_empty() {
                None
            } else {
                Some(joined.join("\n"))
            }
        }
        _ => None,
    }
}

/// Parse one transcript into its normalized event stream.
///
/// Lenient by design: a line that is not valid JSON, or that has an
/// unexpected shape, is skipped rather than failing the parse. Transcripts
/// are written by another program and may be truncated mid-write while a
/// session is live; one bad line must not cost the whole session.
pub fn parse_session(transcript_path: &Path, opts: ParseOptions) -> Result<ParsedSession> {
    let file = std::fs::File::open(transcript_path)?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = std::io::BufReader::new(file);

    let session_id = transcript_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut summary = SessionSummary {
        session_id,
        transcript_path: transcript_path.to_path_buf(),
        cwd: None,
        git_branch: None,
        started_at: None,
        ended_at: None,
        user_turns: 0,
        assistant_turns: 0,
        bytes,
        first_prompt: None,
        skipped_records: 0,
    };
    let mut events: Vec<Event> = Vec::new();
    // tool_use id -> index into `events`, so a later tool_result can patch
    // the call it belongs to in a single forward pass.
    let mut pending: HashMap<String, usize> = HashMap::new();

    // Read record-by-record with a per-record ceiling rather than
    // `reader.lines()`, which would grow one `String` to whatever a single
    // line happens to be. One pasted attachment could otherwise OOM every
    // caller — including `list_sessions_in`, which summarizes *every*
    // transcript in scope, and the SessionEnd hook that falls back to it.
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        // `take` is rebuilt each iteration, so the limit is per record. The
        // `+ 1` is what distinguishes "hit the cap" from "hit EOF mid-record"
        // (a transcript being appended to as we read it).
        let read = reader
            .by_ref()
            .take(MAX_RECORD_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf);
        // A read error must BREAK: it makes no progress, so `continue` here
        // would spin forever.
        let Ok(n) = read else { break };
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') && n > MAX_RECORD_BYTES {
            // Over-long record: discard the rest of it without allocating,
            // so the next `read_until` starts on a record boundary rather
            // than mid-JSON. Skipping matches this parser's existing
            // leniency — a truncated object would not deserialize anyway.
            //
            // Counted, not merely logged: `debug!` sits below the CLI's
            // default level, so the turn counts silently became lower bounds
            // and the digest still called itself complete. `warn!` too, since
            // the one ordinary record class that reaches this size is a pasted
            // screenshot — i.e. a human turn going missing.
            summary.skipped_records += 1;
            tracing::warn!(
                "transcript {}: skipping a record larger than {MAX_RECORD_BYTES} bytes; \
                 its turn is missing from the digest",
                transcript_path.display()
            );
            match reader.skip_until(b'\n') {
                // No newline left: the over-long record ran to EOF.
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            continue;
        };
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let at = record
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        if let Some(at) = at {
            if summary.started_at.is_none() {
                summary.started_at = Some(at);
            }
            summary.ended_at = Some(at);
        }
        if summary.cwd.is_none() {
            summary.cwd = record
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        if summary.git_branch.is_none() {
            summary.git_branch = record
                .get("gitBranch")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }

        let is_sidechain = record
            .get("isSidechain")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_sidechain && !opts.include_sidechains {
            continue;
        }

        let Some(message) = record.get("message") else {
            continue;
        };
        let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");

        match message.get("content") {
            // A bare string is always a human prompt.
            Some(serde_json::Value::String(text)) if role == "user" => {
                push_user_prompt(&mut summary, &mut events, at, text);
            }
            Some(serde_json::Value::Array(blocks)) => {
                for block in blocks {
                    let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match (role, btype) {
                        ("user", "text") => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                push_user_prompt(&mut summary, &mut events, at, text);
                            }
                        }
                        ("user", "tool_result") => {
                            let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            let Some(&idx) = pending.get(id) else {
                                continue;
                            };
                            let is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            if let Some(Event::ToolCall {
                                ok, result_preview, ..
                            }) = events.get_mut(idx)
                            {
                                *ok = Some(!is_error);
                                *result_preview = tool_result_text(block)
                                    .as_deref()
                                    .and_then(|t| first_line_preview(t, RESULT_PREVIEW_CHARS));
                            }
                            pending.remove(id);
                        }
                        ("assistant", "text") => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if !text.trim().is_empty() {
                                    summary.assistant_turns += 1;
                                    events.push(Event::AssistantText {
                                        at,
                                        text: text.trim().to_string(),
                                    });
                                }
                            }
                        }
                        ("assistant", "thinking") => {
                            if !opts.include_thinking {
                                continue;
                            }
                            if let Some(text) = block.get("thinking").and_then(|v| v.as_str()) {
                                if !text.trim().is_empty() {
                                    events.push(Event::Thinking {
                                        at,
                                        text: text.trim().to_string(),
                                    });
                                }
                            }
                        }
                        ("assistant", "tool_use") => {
                            if opts.tools == ToolDetail::None {
                                continue;
                            }
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let target = block.get("input").and_then(tool_target);
                            if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                                pending.insert(id.to_string(), events.len());
                            }
                            events.push(Event::ToolCall {
                                at,
                                name,
                                target,
                                ok: None,
                                result_preview: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Applied here and not at the `tool_use` block above, because that is not
    // where the outcome is known: a call is emitted with `ok: None` and only
    // back-filled when its `tool_result` record arrives, several records
    // later. Filtering at emit time would therefore keep nothing.
    if opts.tools == ToolDetail::FailuresOnly {
        events.retain(|e| !matches!(e, Event::ToolCall { ok, .. } if *ok != Some(false)));
    }

    Ok(ParsedSession { summary, events })
}

/// Record a human prompt, tracking the first one for the session preview.
fn push_user_prompt(
    summary: &mut SessionSummary,
    events: &mut Vec<Event>,
    at: Option<DateTime<Utc>>,
    text: &str,
) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    // Claude Code injects synthetic user turns (command output, hook
    // context, system reminders). They are machine-generated scaffolding,
    // not things the human said, so they must not be mined as intent.
    if is_synthetic_prompt(text) {
        return;
    }
    summary.user_turns += 1;
    if summary.first_prompt.is_none() {
        summary.first_prompt = Some(truncate_chars(text, PROMPT_PREVIEW_CHARS));
    }
    events.push(Event::UserPrompt {
        at,
        text: text.to_string(),
    });
}

/// Detect the machine-generated user turns Claude Code injects.
fn is_synthetic_prompt(text: &str) -> bool {
    // Tag-shaped scaffolding is matched **anywhere**, not just at the start:
    // a prompt that embeds one mid-text is either genuine harness output that
    // was re-pasted, or content forged to look like it, and neither is human
    // intent worth mining. Both the opening and closing forms count, since
    // `</system-reminder>` does not contain `<system-reminder>`.
    const TAG_MARKERS: [&str; 4] = [
        "system-reminder",
        "command-name",
        "local-command-stdout",
        "user-prompt-submit-hook",
    ];
    // Prose markers stay prefix-anchored: these are ordinary enough English
    // that matching them anywhere would silently delete real prompts that
    // merely quote them ("why does the log say [Request interrupted…?").
    const PROSE_MARKERS: [&str; 2] = [
        "Caveat: The messages below were generated",
        "[Request interrupted",
    ];

    // Prefix-anchored, deliberately. Matching a tag *anywhere* deletes the
    // whole turn, and a turn is legitimately allowed to mention one — "why
    // does hook.rs emit <system-reminder> twice?" is a real question, and
    // dropping it also miscounts `user_turns` and shifts `first_prompt`
    // while still reporting the digest complete. Tags embedded mid-prompt are
    // handled where they actually matter, by `ops::harvest`'s defang, which
    // neutralizes them without discarding the human's words.
    //
    // That claim is load-bearing and was for a while only half true: the
    // digest render defanged, and the `harvest_list` / `harvest_search` /
    // `harvest_ledger` listings — which quote `first_prompt`, `git_branch` and
    // `cwd` out of the very same records — applied `sanitize_one_line` alone,
    // which has never touched a tag. Every consumer of this stream now goes
    // through `ops::harvest::defang_metadata` or the digest's `defang`; a new
    // one that does not re-opens this hole rather than creating a new one.
    // Strip invisibles first: this runs on *raw* record text, so without it
    // `<system\u{200d}-reminder>` reads as an ordinary human turn.
    let cleaned: String = text.chars().filter(|c| !is_invisible_format(*c)).collect();
    let lowered = cleaned.trim_start().to_lowercase();
    TAG_MARKERS
        .iter()
        .any(|m| lowered.starts_with(&format!("<{m}")) || lowered.starts_with(&format!("</{m}")))
        || PROSE_MARKERS.iter().any(|m| text.starts_with(m))
}

/// Cheap metadata-only scan used by the session listing.
pub fn summarize_session(transcript_path: &Path) -> Result<SessionSummary> {
    // Reuse the full parse with tools/thinking off: the event vector stays
    // small (prompts + prose only) and the counts come out identical.
    let opts = ParseOptions {
        include_thinking: false,
        include_sidechains: false,
        tools: ToolDetail::None,
    };
    Ok(parse_session(transcript_path, opts)?.summary)
}

/// List every transcript that belongs to one of `project_paths`.
///
/// Attribution is by the `cwd` recorded *inside* each transcript, with the
/// encoded directory name used only to avoid scanning unrelated project
/// directories. When `project_paths` is empty, every transcript under
/// [`projects_root`] is returned.
///
/// Results are sorted newest-activity-first. A missing projects root yields
/// an empty list rather than an error: Claude Code may simply never have run
/// on this machine, which is not a failure of the harvest command.
pub fn list_sessions_for(project_paths: &[PathBuf]) -> Result<Vec<SessionSummary>> {
    list_sessions_in(&projects_root()?, project_paths)
}

/// [`list_sessions_for`] against an explicit projects root.
///
/// The root is a parameter rather than an ambient lookup so tests can point
/// at a fixture directory without mutating `CLAUDE_CONFIG_DIR` — a process
/// global that would make these tests order-dependent.
pub fn list_sessions_in(root: &Path, project_paths: &[PathBuf]) -> Result<Vec<SessionSummary>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    // Canonicalize the wanted paths once so a symlinked checkout (or a
    // trailing-slash spelling) still matches the recorded cwd.
    let wanted: Vec<PathBuf> = project_paths
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    let encoded: Vec<String> = wanted.iter().map(|p| encode_project_dir(p)).collect();
    // The encoded name is only trustworthy while the path is ASCII (see
    // `encode_project_dir`). A disagreement is not a near miss: the prefix
    // test below `continue`s, and the directory's sessions become invisible
    // for good. So for a non-ASCII path the fast path stands down and the
    // recorded `cwd` — authoritative regardless — does all the work.
    let fast_path_ok = wanted.iter().all(|p| p.to_string_lossy().is_ascii());

    // Enumerated first, parsed second: the walk is a handful of `read_dir`
    // calls and has to be sequential anyway, while the parse is the part worth
    // spreading across cores.
    let mut out: Vec<(PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Fast path: skip directories that cannot encode to any wanted path.
        // Only an optimization — the cwd check below is what decides.
        //
        // A **prefix** test, not equality: Claude Code names the directory
        // after the session's cwd, which for a session started in a
        // subdirectory is longer than the project root. Requiring equality
        // made every such session permanently invisible to harvest, since
        // this fast path runs before `cwd_matches` ever gets to decide.
        // `exact_dir` records whether the directory encodes a wanted root
        // *exactly*. A transcript with no recorded `cwd` cannot be attributed
        // on its own, and is accepted only in that narrow case — under the
        // prefix rule alone, `/repo-other` starts with `encode("/repo")`, so
        // a blanket accept would feed a sibling project's conversations into
        // this project's harvest.
        let name = entry.file_name().to_string_lossy().to_string();
        let mut exact_dir = true;
        if !wanted.is_empty() {
            if fast_path_ok {
                if !encoded.iter().any(|e| name.starts_with(e.as_str())) {
                    continue;
                }
                exact_dir = encoded.contains(&name);
            } else {
                // No trustworthy name evidence, so a transcript carrying no
                // `cwd` must not be attributed on a name we cannot reproduce.
                exact_dir = false;
            }
        }

        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // Enumeration only. The parse is the expensive half and it is
            // deferred to the parallel pass below; `exact_dir` travels with
            // the path because it is a property of the *directory*, and the
            // attribution rule needs it per file.
            out.push((path, exact_dir));
        }
    }

    // Every transcript is an independent parse of an independent file, so this
    // is embarrassingly parallel CPU work — `summarize_session` reads the
    // whole JSONL and deserializes every record, and `harvest list` is
    // interactive. Rayon's default pool is sized to the CPU count, which is
    // also what bounds peak memory: each in-flight parse holds at most one
    // `MAX_RECORD_BYTES` (4 MiB) line buffer plus the session's prose events,
    // so the ceiling is per-core rather than per-transcript.
    let mut out: Vec<SessionSummary> = out
        .into_par_iter()
        .filter_map(|(path, exact_dir)| {
            let summary = summarize_session(&path).ok()?;
            if !wanted.is_empty() && !cwd_matches(summary.cwd.as_deref(), &wanted, exact_dir) {
                return None;
            }
            Some(summary)
        })
        .collect();

    // Newest first, session id as the tie-break. The tie-break is load-bearing
    // rather than cosmetic: `sort_by_key` is stable, so before this the order
    // of two sessions sharing an `ended_at` (or both missing one) was whatever
    // order `read_dir` happened to yield — and `harvest list --limit` and
    // `index_pending`'s budget both cut through that order. Making it total
    // means the parallel collect above cannot change the answer, and neither
    // can the filesystem.
    out.sort_by(|a, b| {
        b.ended_at
            .cmp(&a.ended_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(out)
}

/// Does a transcript's recorded `cwd` belong to one of the wanted projects?
///
/// A session started in a subdirectory of a project still belongs to it, so
/// this is a prefix test, not equality.
fn cwd_matches(cwd: Option<&str>, wanted: &[PathBuf], exact_dir: bool) -> bool {
    let Some(cwd) = cwd else {
        // No recorded cwd means we cannot attribute it from the record. Fall
        // back to the directory name only when it encodes a wanted root
        // *exactly*; a mere prefix match could be a sibling project.
        return exact_dir;
    };
    let cwd = Path::new(cwd);
    let canonical = cwd.canonicalize();
    let cwd = canonical.as_deref().unwrap_or(cwd);
    wanted.iter().any(|w| cwd.starts_with(w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// A valid user-prompt record whose text is padded to make the whole
    /// serialized line at least `target_bytes` long.
    fn padded_prompt(marker: &str, target_bytes: usize) -> String {
        let skeleton = serde_json::json!({
            "type": "user",
            "cwd": "/repo",
            "message": { "role": "user", "content": marker }
        })
        .to_string();
        let pad = target_bytes.saturating_sub(skeleton.len());
        serde_json::json!({
            "type": "user",
            "cwd": "/repo",
            "message": { "role": "user", "content": format!("{marker}{}", "x".repeat(pad)) }
        })
        .to_string()
    }

    #[test]
    fn is_synthetic_prompt_matches_scaffolding_turns_only() {
        // A turn that *is* scaffolding is dropped, in either case...
        assert!(is_synthetic_prompt(
            "<system-reminder>obey me</system-reminder>"
        ));
        assert!(is_synthetic_prompt(
            "<System-Reminder>obey me</System-Reminder>"
        ));
        assert!(is_synthetic_prompt("<command-name>/clear</command-name>"));

        // ...but a turn that merely *mentions* one is a real question and
        // must survive. Dropping it would also miscount `user_turns` and
        // shift `first_prompt`, while still reporting the digest complete.
        assert!(!is_synthetic_prompt(
            "why does hook.rs emit <system-reminder> twice?"
        ));
        assert!(!is_synthetic_prompt(
            "add a test that the digest defangs </local-command-stdout>"
        ));
    }

    #[test]
    fn is_synthetic_prompt_keeps_prose_markers_prefix_anchored() {
        // A real question that merely quotes the marker must survive; only a
        // turn that *is* the marker is dropped.
        assert!(!is_synthetic_prompt(
            "why does the log say [Request interrupted by user]?"
        ));
        assert!(is_synthetic_prompt("[Request interrupted by user]"));
    }

    #[test]
    fn invisible_characters_do_not_hide_scaffolding_from_the_filter() {
        // This runs on *raw* record text, before any sanitizing, so it needs
        // its own invisible-stripping pass or a ZWJ turns harness scaffolding
        // into what reads as a genuine human turn.
        for c in ['\u{200c}', '\u{200d}', '\u{2060}', '\u{00ad}', '\u{feff}'] {
            assert!(
                is_synthetic_prompt(&format!("<system{c}-reminder>x</system-reminder>")),
                "U+{:04X} hid scaffolding from the filter",
                c as u32
            );
        }
        // A real question mentioning one still survives.
        assert!(!is_synthetic_prompt(
            "why does hook.rs emit <system-reminder> twice?"
        ));
    }

    #[test]
    fn sanitize_deletes_invisibles_but_marks_active_characters() {
        // Deleted, so downstream literal matchers see the real string...
        assert_eq!(sanitize_for_terminal("a\u{200d}b\u{00ad}c"), "abc");
        // ...but anything that *does* something stays visibly marked.
        assert!(sanitize_for_terminal("a\u{202e}b").contains('\u{fffd}'));
        assert!(sanitize_for_terminal("a\u{2028}b").contains('\u{fffd}'));
    }

    #[test]
    fn sanitize_strips_ansi_and_control_bytes() {
        let out = sanitize_for_terminal("safe\x1b[31mred\x1b[0m");
        assert!(!out.contains('\x1b'), "ESC survived: {out:?}");
        assert!(out.contains("safe") && out.contains("red"), "{out:?}");
        for hostile in ["\x07", "\x00", "\x7f", "\u{9b}", "\u{9d}"] {
            assert!(
                sanitize_for_terminal(hostile).contains('\u{fffd}'),
                "{hostile:?} was not neutralized"
            );
        }
        // Newlines and tabs are legitimate digest structure.
        assert_eq!(sanitize_for_terminal("a\nb\tc"), "a\nb\tc");
    }

    #[test]
    fn sanitize_strips_bidi_overrides() {
        // Trojan Source: an override can make a preview render in an order
        // that inverts what it actually says.
        let out = sanitize_for_terminal("rm -rf \u{202e}gnp. \u{202d}");
        assert!(
            !out.contains('\u{202e}') && !out.contains('\u{202d}'),
            "{out:?}"
        );
    }

    #[test]
    fn sanitize_one_line_collapses_newlines_and_lone_cr() {
        let out = sanitize_one_line("real question\n2026-01-01 00:00  9 turns");
        assert!(!out.contains('\n'), "a forged extra row survived: {out:?}");
        assert_eq!(sanitize_one_line("a\r\nb"), "a b");
        assert!(
            sanitize_for_terminal("a\rb").contains('\u{fffd}'),
            "a lone CR must not survive to reset the line"
        );
    }

    #[test]
    fn sanitize_is_borrowed_for_clean_input() {
        // Guards the no-allocation fast path that makes it safe to call this
        // on every row of a long listing.
        assert!(matches!(
            sanitize_for_terminal("plain ascii prompt"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn sanitize_preserves_multibyte_text() {
        for text in [
            "日本語のテキスト",
            "emoji 🎉 works",
            "café combining e\u{301}",
        ] {
            assert_eq!(sanitize_for_terminal(text), text, "mangled {text:?}");
        }
    }

    #[test]
    fn oversized_record_is_skipped_without_losing_the_rest() {
        // The anti-desync assertion: the record *after* the oversized one must
        // still parse, which only holds if the remainder of the long line was
        // discarded to its newline rather than left mid-JSON.
        let tmp = TempDir::new().unwrap();
        let huge = padded_prompt("HUGE", MAX_RECORD_BYTES + 4096);
        let path = write_transcript(
            tmp.path(),
            "s",
            &[
                &padded_prompt("FIRST", 0),
                &huge,
                &padded_prompt("THIRD", 0),
            ],
        );

        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(parsed.summary.user_turns, 2, "oversized record must be cut");
        assert_eq!(
            parsed.summary.skipped_records, 1,
            "a dropped record must be counted, or `user_turns` is a silent lower bound"
        );
        let texts: Vec<&str> = parsed
            .events
            .iter()
            .filter_map(|e| match e {
                Event::UserPrompt { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts[0].starts_with("FIRST"));
        assert!(
            texts[1].starts_with("THIRD"),
            "the record after an oversized one was lost: {texts:?}"
        );
    }

    #[test]
    fn oversized_final_record_terminates_the_scan() {
        // Over-long record with no trailing newline: `skip_until` finds no
        // delimiter, so the loop must break rather than spin.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let path = tmp.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", padded_prompt("FIRST", 0)).unwrap();
        write!(f, "{}", padded_prompt("HUGE", MAX_RECORD_BYTES + 4096)).unwrap();
        drop(f);

        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(parsed.summary.user_turns, 1);
        assert_eq!(
            parsed.summary.skipped_records, 1,
            "a record that ran to EOF over the cap is still a dropped record"
        );
    }

    #[test]
    fn record_at_exactly_the_cap_is_kept() {
        // Pins the off-by-one from BOTH sides. The newline-terminated case
        // alone is not enough: `buf.last() != Some(&b'\n')` rescues it even
        // if the length test is wrong, so an unterminated exact-cap record —
        // where only the length test can save it — is the real oracle.
        let tmp = TempDir::new().unwrap();
        let exact = padded_prompt("EXACT", MAX_RECORD_BYTES);
        assert_eq!(exact.len(), MAX_RECORD_BYTES, "fixture must sit on the cap");
        let path = write_transcript(tmp.path(), "s", &[&exact]);
        assert_eq!(
            parse_session(&path, ParseOptions::default())
                .unwrap()
                .summary
                .user_turns,
            1
        );

        // Same size, no trailing newline.
        let bare = tmp.path().join("bare.jsonl");
        std::fs::write(&bare, &exact).unwrap();
        assert_eq!(
            parse_session(&bare, ParseOptions::default())
                .unwrap()
                .summary
                .user_turns,
            1,
            "an unterminated record of exactly the cap must still parse"
        );

        // One byte over, unterminated: must be dropped.
        let over = tmp.path().join("over.jsonl");
        std::fs::write(&over, padded_prompt("OVER", MAX_RECORD_BYTES + 1)).unwrap();
        assert_eq!(
            parse_session(&over, ParseOptions::default())
                .unwrap()
                .summary
                .user_turns,
            0,
            "cap+1 must be over the limit"
        );
    }

    #[test]
    fn crlf_and_invalid_utf8_lines_behave_as_before() {
        // Regression guard for replacing `reader.lines()`, which stripped a
        // trailing `\r` and skipped undecodable lines.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let path = tmp.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "{}\r\n", padded_prompt("CRLF", 0)).unwrap();
        f.write_all(&[0xff, 0xfe, b'\n']).unwrap();
        writeln!(f, "{}", padded_prompt("AFTER", 0)).unwrap();
        drop(f);

        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(
            parsed.summary.user_turns, 2,
            "CRLF record and the one after invalid UTF-8 must both parse"
        );
        // The CR must actually be stripped, not merely tolerated by
        // serde_json — otherwise this half of the test proves nothing.
        match &parsed.events[0] {
            Event::UserPrompt { text, .. } => assert!(
                !text.contains('\r') && text.starts_with("CRLF"),
                "trailing CR leaked into the parsed text: {text:?}"
            ),
            other => panic!("expected a user prompt, got {other:?}"),
        }
    }

    /// The projection the search index is built from: tool noise dilutes the
    /// vector, but a failure and its error text is frequently the whole
    /// durable lesson. Success/failure is only known when the *result* record
    /// arrives, so filtering at the `tool_use` block would keep nothing.
    #[test]
    fn failures_only_keeps_failed_calls_and_drops_successful_ones() {
        let tmp = TempDir::new().unwrap();
        let assistant = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "trying"},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo build"}},
                {"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/a.rs"}},
            ]},
        })
        .to_string();
        let results = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "is_error": true,
                 "content": "error: could not find protoc"},
                {"type": "tool_result", "tool_use_id": "t2", "is_error": false, "content": "ok"},
            ]},
        })
        .to_string();
        let path = write_transcript(tmp.path(), "s", &[&assistant, &results]);

        let all = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(
            all.events
                .iter()
                .filter(|e| matches!(e, Event::ToolCall { .. }))
                .count(),
            2,
            "the default profile keeps the whole trace"
        );

        let failures = parse_session(
            &path,
            ParseOptions {
                tools: ToolDetail::FailuresOnly,
                ..Default::default()
            },
        )
        .unwrap();
        let tools: Vec<&Event> = failures
            .events
            .iter()
            .filter(|e| matches!(e, Event::ToolCall { .. }))
            .collect();
        assert_eq!(tools.len(), 1, "{tools:?}");
        match tools[0] {
            Event::ToolCall {
                name,
                ok,
                result_preview,
                ..
            } => {
                assert_eq!(name, "Bash");
                assert_eq!(*ok, Some(false));
                assert!(
                    result_preview
                        .as_deref()
                        .is_some_and(|p| p.contains("could not find protoc")),
                    "the error text must survive: {result_preview:?}"
                );
            }
            other => panic!("expected a tool call, got {other:?}"),
        }
        // Prose is untouched by the profile.
        assert!(failures
            .events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { .. })));

        let none = parse_session(
            &path,
            ParseOptions {
                tools: ToolDetail::None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(none
            .events
            .iter()
            .all(|e| !matches!(e, Event::ToolCall { .. })));
    }

    /// A call whose result never arrived is not a *known* failure — the
    /// session was cut off mid-call — so it must not be reported as one.
    #[test]
    fn failures_only_drops_a_call_with_no_result() {
        let tmp = TempDir::new().unwrap();
        let assistant = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 1"}},
            ]},
        })
        .to_string();
        let path = write_transcript(tmp.path(), "s", &[&assistant]);
        let parsed = parse_session(
            &path,
            ParseOptions {
                tools: ToolDetail::FailuresOnly,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(parsed
            .events
            .iter()
            .all(|e| !matches!(e, Event::ToolCall { .. })));
    }

    fn write_transcript(dir: &Path, name: &str, lines: &[&str]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.jsonl"));
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn encode_project_dir_matches_claude_code_layout() {
        assert_eq!(
            encode_project_dir(Path::new("/home/user/engramdb")),
            "-home-user-engramdb"
        );
        // Lossy on purpose: dots and dashes collapse to the same name.
        assert_eq!(
            encode_project_dir(Path::new("/a/b.c")),
            encode_project_dir(Path::new("/a/b-c"))
        );
    }

    #[test]
    fn parses_prompts_prose_and_tool_calls() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            "s1",
            &[
                r#"{"type":"user","cwd":"/repo","gitBranch":"main","timestamp":"2026-07-31T10:00:00Z","message":{"role":"user","content":"why does the build fail"}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-31T10:00:05Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Checking the build."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo build"}}]}}"#,
                r#"{"type":"user","timestamp":"2026-07-31T10:00:09Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"error: linker not found\nmore detail"}]}}"#,
            ],
        );

        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(parsed.summary.cwd.as_deref(), Some("/repo"));
        assert_eq!(parsed.summary.git_branch.as_deref(), Some("main"));
        assert_eq!(parsed.summary.user_turns, 1);
        assert_eq!(parsed.summary.assistant_turns, 1);
        assert!(parsed.summary.first_prompt.unwrap().starts_with("why does"));

        // Thinking is excluded by default.
        assert_eq!(parsed.events.len(), 3);
        assert!(matches!(parsed.events[0], Event::UserPrompt { .. }));
        assert!(matches!(parsed.events[1], Event::AssistantText { .. }));
        match &parsed.events[2] {
            Event::ToolCall {
                name,
                target,
                ok,
                result_preview,
                ..
            } => {
                assert_eq!(name, "Bash");
                assert_eq!(target.as_deref(), Some("cargo build"));
                // The failure flag is the whole point of keeping results.
                assert_eq!(*ok, Some(false));
                assert_eq!(result_preview.as_deref(), Some("error: linker not found"));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn include_thinking_opt_in() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            "s1",
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"reasoning here"}]}}"#,
            ],
        );
        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert!(parsed.events.is_empty());

        let opts = ParseOptions {
            include_thinking: true,
            ..Default::default()
        };
        let parsed = parse_session(&path, opts).unwrap();
        assert!(matches!(parsed.events[0], Event::Thinking { .. }));
    }

    #[test]
    fn skips_sidechains_and_synthetic_prompts_and_bad_lines() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            "s1",
            &[
                "not json at all",
                r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"subagent prompt"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<command-name>/reflect</command-name>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<system-reminder>be nice</system-reminder>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"a real question"}}"#,
            ],
        );
        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(parsed.summary.user_turns, 1);
        assert_eq!(parsed.events.len(), 1);
        match &parsed.events[0] {
            Event::UserPrompt { text, .. } => assert_eq!(text, "a real question"),
            other => panic!("expected prompt, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_tool_call_reports_unknown_outcome() {
        let tmp = TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            "s1",
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/repo/src/lib.rs"}}]}}"#,
            ],
        );
        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        match &parsed.events[0] {
            Event::ToolCall { ok, target, .. } => {
                assert_eq!(*ok, None);
                assert_eq!(target.as_deref(), Some("/repo/src/lib.rs"));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn list_sessions_attributes_by_recorded_cwd() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join("claude");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();

        // The encoded directory name for the repo, holding two transcripts:
        // one really from the repo, one whose recorded cwd is elsewhere (the
        // lossy-encoding collision this guards against).
        let encoded = encode_project_dir(&repo);
        let dir = claude.join("projects").join(&encoded);
        write_transcript(
            &dir,
            "mine",
            &[&format!(
                r#"{{"type":"user","cwd":"{}","timestamp":"2026-07-31T10:00:00Z","message":{{"role":"user","content":"mine"}}}}"#,
                repo.display()
            )],
        );
        write_transcript(
            &dir,
            "theirs",
            &[
                r#"{"type":"user","cwd":"/somewhere/else","timestamp":"2026-07-31T11:00:00Z","message":{"role":"user","content":"theirs"}}"#,
            ],
        );

        let found =
            list_sessions_in(&claude.join("projects"), std::slice::from_ref(&repo)).unwrap();
        assert_eq!(found.len(), 1, "collided transcript must be rejected");
        assert_eq!(found[0].session_id, "mine");
    }

    #[test]
    fn list_sessions_matches_subdirectory_cwd() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join("claude");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("crates")).unwrap();
        let repo = repo.canonicalize().unwrap();

        let dir = claude.join("projects").join(encode_project_dir(&repo));
        write_transcript(
            &dir,
            "sub",
            &[&format!(
                r#"{{"type":"user","cwd":"{}/crates","message":{{"role":"user","content":"hi"}}}}"#,
                repo.display()
            )],
        );

        let found =
            list_sessions_in(&claude.join("projects"), std::slice::from_ref(&repo)).unwrap();
        assert_eq!(found.len(), 1);
    }

    /// The parse is spread across cores, so nothing may depend on the order
    /// the transcripts happened to be enumerated in.
    ///
    /// The listing is what `harvest list --limit`, `pending_sessions`' budget
    /// and the archive sweep all cut through, and `sort_by_key` is *stable* —
    /// so before the total ordering below, two sessions sharing an `ended_at`
    /// came back in whatever order `read_dir` yielded, which is filesystem
    /// state, not data. Twenty sessions on one timestamp: every call must
    /// produce the identical sequence.
    #[test]
    fn sessions_sharing_an_end_time_come_back_in_a_stable_order() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join("claude");
        let dir = claude.join("projects").join("-repo");
        for i in 0..20 {
            write_transcript(
                &dir,
                &format!("s{i:02}"),
                &[
                    r#"{"type":"user","cwd":"/repo","timestamp":"2026-07-31T10:00:00Z","message":{"role":"user","content":"same instant"}}"#,
                ],
            );
        }

        let first: Vec<String> = list_sessions_in(&claude.join("projects"), &[])
            .unwrap()
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(
            first.len(),
            20,
            "every transcript must be parsed exactly once"
        );
        let mut expected = first.clone();
        expected.sort();
        assert_eq!(
            first, expected,
            "sessions on one timestamp must fall back to the session id, not to read_dir order"
        );

        // Repeated calls agree — a parallel collect that leaked into the
        // result would show up here as a differing permutation.
        for _ in 0..5 {
            let again: Vec<String> = list_sessions_in(&claude.join("projects"), &[])
                .unwrap()
                .into_iter()
                .map(|s| s.session_id)
                .collect();
            assert_eq!(again, first);
        }
    }

    /// Newest-first is the primary key and must survive the parallel pass.
    #[test]
    fn sessions_are_still_ordered_newest_first() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join("claude");
        let dir = claude.join("projects").join("-repo");
        for (session, ts) in [
            ("oldest", "2026-07-01T10:00:00Z"),
            ("middle", "2026-07-15T10:00:00Z"),
            ("newest", "2026-07-31T10:00:00Z"),
        ] {
            write_transcript(
                &dir,
                session,
                &[&format!(
                    r#"{{"type":"user","cwd":"/repo","timestamp":"{ts}","message":{{"role":"user","content":"hi"}}}}"#
                )],
            );
        }
        let found: Vec<String> = list_sessions_in(&claude.join("projects"), &[])
            .unwrap()
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(found, vec!["newest", "middle", "oldest"]);
    }

    /// `exact_dir` is a property of the *directory*, and the parallel pass
    /// moved the attribution decision away from the loop that computes it. If
    /// it stopped travelling with the file, a sibling project whose encoded
    /// name merely *prefixes* the wanted one would start contributing its
    /// undated transcripts.
    #[test]
    fn a_prefix_named_sibling_directory_still_does_not_donate_undated_sessions() {
        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join("claude");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        let encoded = encode_project_dir(&repo);

        // Exact directory, no recorded cwd: attributable on the name alone.
        write_transcript(
            &claude.join("projects").join(&encoded),
            "ours",
            &[
                r#"{"type":"user","timestamp":"2026-07-31T10:00:00Z","message":{"role":"user","content":"hi"}}"#,
            ],
        );
        // Prefix-named sibling, no recorded cwd: must NOT be attributed.
        write_transcript(
            &claude.join("projects").join(format!("{encoded}-other")),
            "theirs",
            &[
                r#"{"type":"user","timestamp":"2026-07-31T11:00:00Z","message":{"role":"user","content":"hi"}}"#,
            ],
        );

        let found =
            list_sessions_in(&claude.join("projects"), std::slice::from_ref(&repo)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "ours");
    }

    #[test]
    fn missing_projects_root_is_empty_not_error() {
        let tmp = TempDir::new().unwrap();
        assert!(list_sessions_in(&tmp.path().join("nonexistent"), &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn claude_home_honors_config_dir_override() {
        // `claude_home` is the only ambient-lookup entry point; the listing
        // functions take an explicit root precisely so this is the one place
        // the env var needs covering.
        let key = "CLAUDE_CONFIG_DIR";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/custom/claude");
        assert_eq!(claude_home().unwrap(), PathBuf::from("/custom/claude"));
        assert_eq!(
            projects_root().unwrap(),
            PathBuf::from("/custom/claude/projects")
        );
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
