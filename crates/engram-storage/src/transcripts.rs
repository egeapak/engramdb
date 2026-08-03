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
/// records): p50 1.5 KB, p99 33 KB, largest 95 KB. 1 MiB is ~11x the largest
/// observed, so no realistic record is lost, while bounding the cost of one
/// hostile or pathological line — a huge pasted attachment is the only record
/// class Claude Code does not itself truncate.
const MAX_RECORD_BYTES: usize = 1_048_576;

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
/// directory: every byte outside `[A-Za-z0-9]` becomes `-`.
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

/// What to keep when parsing a transcript.
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    /// Include assistant reasoning blocks. Default `false`.
    pub include_thinking: bool,
    /// Include subagent (`isSidechain`) turns. Default `false`: subagents
    /// report their findings back into the main thread, so their raw turns
    /// are mostly duplicate volume.
    pub include_sidechains: bool,
    /// Include tool calls. Default `true` — the sequence of actions is often
    /// where a convention or hazard is visible.
    pub include_tools: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_sidechains: false,
            include_tools: true,
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
            tracing::debug!(
                "transcript {}: skipping a record larger than {MAX_RECORD_BYTES} bytes",
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
                            if !opts.include_tools {
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
    const MARKERS: [&str; 6] = [
        "<command-name>",
        "<local-command-stdout>",
        "<system-reminder>",
        "Caveat: The messages below were generated",
        "[Request interrupted",
        "<user-prompt-submit-hook>",
    ];
    MARKERS.iter().any(|m| text.starts_with(m))
}

/// Cheap metadata-only scan used by the session listing.
pub fn summarize_session(transcript_path: &Path) -> Result<SessionSummary> {
    // Reuse the full parse with tools/thinking off: the event vector stays
    // small (prompts + prose only) and the counts come out identical.
    let opts = ParseOptions {
        include_thinking: false,
        include_sidechains: false,
        include_tools: false,
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

    let mut out = Vec::new();
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
            if !encoded.iter().any(|e| name.starts_with(e.as_str())) {
                continue;
            }
            exact_dir = encoded.contains(&name);
        }

        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(summary) = summarize_session(&path) else {
                continue;
            };
            if !wanted.is_empty() && !cwd_matches(summary.cwd.as_deref(), &wanted, exact_dir) {
                continue;
            }
            out.push(summary);
        }
    }

    out.sort_by_key(|s| std::cmp::Reverse(s.ended_at));
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
    }

    #[test]
    fn record_at_exactly_the_cap_is_kept() {
        // Pins the off-by-one: a record of exactly MAX_RECORD_BYTES (plus its
        // newline) is under the limit, not over it.
        let tmp = TempDir::new().unwrap();
        let exact = padded_prompt("EXACT", MAX_RECORD_BYTES);
        assert_eq!(exact.len(), MAX_RECORD_BYTES, "fixture must sit on the cap");
        let path = write_transcript(tmp.path(), "s", &[&exact]);

        let parsed = parse_session(&path, ParseOptions::default()).unwrap();
        assert_eq!(parsed.summary.user_turns, 1);
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
