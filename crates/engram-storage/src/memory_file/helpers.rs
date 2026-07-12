//! Shared parsing helpers used by multiple format versions.
//!
//! # Body-text escaping
//!
//! Memory content routinely contains markdown, so a content line could look
//! exactly like the structure the writer emits (`# `, `## `, `<!--`, `-->`,
//! `> `). To keep files round-trip-safe, the writers escape such lines with a
//! single leading backslash (`## Scope` is written as `\## Scope`) and the
//! parser strips that one backslash back off when accumulating section text.
//! A line that already starts with backslashes followed by a structural prefix
//! gains one more backslash (`\## x` → `\\## x`), so user content that
//! literally contains the escape survives unchanged.
//!
//! Files written before this scheme existed contain no escapes; they parse
//! exactly as before unless their content happened to look structural — and
//! those files were already mis-parsed, so nothing regresses.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::error::{Result, StorageError};
use engram_types::{Provenance, ProvenanceSource};

/// Opening marker line of the writer-emitted hidden-metadata block.
pub const HIDDEN_META_START: &str = "<!-- engramdb";
/// Closing marker line of the writer-emitted hidden-metadata block.
pub const HIDDEN_META_END: &str = "-->";

/// Split a memory file into `(frontmatter, body)`.
///
/// The opening fence must be the first non-blank line and be exactly `---`;
/// the closing fence is the next line that is exactly `---` (a trailing `\r`
/// is tolerated for CRLF files). Scanning line-wise means a `---` appearing
/// *inside* a YAML value (e.g. a title) or in the body can never be mistaken
/// for a fence — unlike a textual `splitn(3, "---")`.
pub fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let mut fm_start: Option<usize> = None;
    let mut offset = 0;

    for raw in content.split_inclusive('\n') {
        let line_start = offset;
        offset += raw.len();
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');

        match fm_start {
            None => {
                if line.trim().is_empty() {
                    continue; // tolerate leading blank lines
                }
                if line == "---" {
                    fm_start = Some(offset);
                } else {
                    return Err(StorageError::InvalidFormat(
                        "Missing frontmatter".to_string(),
                    ));
                }
            }
            Some(start) => {
                if line == "---" {
                    let frontmatter = content[start..line_start].trim();
                    let body = &content[offset..];
                    return Ok((frontmatter, body));
                }
            }
        }
    }

    Err(StorageError::InvalidFormat(if fm_start.is_none() {
        "Missing frontmatter".to_string()
    } else {
        "Missing body after frontmatter".to_string()
    }))
}

/// Whether `parse_body_sections` would treat this line as structure rather
/// than plain section text.
fn is_structural_line(line: &str) -> bool {
    line.starts_with("# ")
        || line.starts_with("## ")
        || line.starts_with("<!--")
        || line.starts_with("-->")
        || line.starts_with("> ")
        || line == ">"
}

/// Escape free text (content/details) so none of its lines can be mistaken
/// for structure on re-parse. See the module docs for the scheme.
pub fn escape_body_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    for raw in text.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if is_structural_line(line.trim_start_matches('\\')) {
            out.push('\\');
        }
        out.push_str(raw);
    }
    out
}

/// Collapse a value that the writer emits on a single structural line
/// (`# {heading}`, `**Summary:** {v}`, `- **Reason:** {v}`, …) down to one
/// line: newlines become spaces. Without this, a multi-line summary/title/
/// provenance value injects raw lines into the body — a value containing
/// `\n## Content` writes a fake section heading that, on re-parse, shadows
/// the real content (first-occurrence-wins), silently replacing it.
pub fn sanitize_single_line(value: &str) -> String {
    if !value.contains(['\n', '\r']) {
        return value.to_string();
    }
    value
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Sanitize one item of a backtick-delimited list field (Files/Logical/Tags):
/// collapse newlines like any single-line field, and strip backticks — the
/// backtick is the item delimiter, so an embedded one would desynchronize the
/// writer and parser (write→parse→write must be a fixed point).
pub fn sanitize_list_item(item: &str) -> String {
    sanitize_single_line(item).replace('`', "")
}

/// Reverse of [`escape_body_text`] for a single line: strip exactly one
/// leading backslash, but only when it guards a structural prefix (so
/// genuinely-backslashed user content is left intact).
fn unescape_body_line(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('\\') {
        if is_structural_line(line.trim_start_matches('\\')) {
            return rest;
        }
    }
    line
}

/// Parse the body into named sections. Recognizes:
/// - `# heading` → stored under key `__h1__` (only the first, and only in the
///   preamble before any `## ` section — matching the writer's layout)
/// - `> blockquote` → stored under key `__blockquote__`
/// - `## SectionName` → stored under key `SectionName` (first occurrence wins;
///   the writer emits each section exactly once)
/// - the writer-emitted `<!-- engramdb ... -->` block is skipped entirely
///   (parsed separately by `parse_hidden_meta`)
///
/// Escaped lines (see module docs) are unescaped as they are accumulated.
pub fn parse_body_sections(body: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
    let mut current_section: Option<String> = None;
    let mut current_content = Vec::new();
    let mut in_hidden_block = false;

    for line in body.lines() {
        // Writer-emitted hidden-metadata block: skip it as a fenced region.
        if in_hidden_block {
            if line.trim_end() == HIDDEN_META_END {
                in_hidden_block = false;
            }
            continue;
        }
        if line.trim_end() == HIDDEN_META_START {
            in_hidden_block = true;
            continue;
        }

        // H1 heading: only the first one, in the preamble before any section.
        // The writer emits exactly one H1 at the top of the body; anything
        // that looks like an H1 later is content (old unescaped files).
        if current_section.is_none() && !sections.contains_key("__h1__") {
            if let Some(heading) = line.strip_prefix("# ") {
                sections.insert("__h1__".to_string(), heading.trim().to_string());
                continue;
            }
        }

        // Blockquote (collect `>` lines) — ONLY in the preamble, before any
        // `## ` section. Inside a section a `> ` line is ordinary content and
        // must stay there; harvesting it everywhere silently dropped quoted
        // lines from `## Content`/`## Details` (finding #6). No writer ever
        // emits a blockquote, so this branch only ever matched hand-edited or
        // legacy files.
        if current_section.is_none() && (line.starts_with("> ") || line == ">") {
            let text = line.strip_prefix("> ").unwrap_or("").trim();
            if let Some(existing) = sections.get_mut("__blockquote__") {
                existing.push(' ');
                existing.push_str(text);
            } else {
                sections.insert("__blockquote__".to_string(), text.to_string());
            }
            continue;
        }

        // H2 section
        if let Some(header) = line.strip_prefix("## ") {
            flush_section(&mut sections, &mut current_section, &mut current_content);
            current_section = Some(header.trim().to_string());
            continue;
        }

        // Skip stray HTML comment lines (legacy files; new writers escape
        // comment lines inside content so they reach the branch below).
        if line.starts_with("<!--") || line.starts_with("-->") {
            continue;
        }

        // Accumulate lines into current section, undoing writer escaping.
        if current_section.is_some() {
            current_content.push(unescape_body_line(line));
        }
    }

    flush_section(&mut sections, &mut current_section, &mut current_content);
    sections
}

fn flush_section(
    sections: &mut HashMap<String, String>,
    current_section: &mut Option<String>,
    current_content: &mut Vec<&str>,
) {
    if let Some(name) = current_section.take() {
        let text = current_content.join("\n").trim().to_string();
        // First occurrence wins: the writer emits each section once, so a
        // duplicate heading can only come from content in old unescaped files
        // (or hand edits) and must not override the real section.
        sections.entry(name).or_insert(text);
        current_content.clear();
    }
}

/// Match a `**Field:** value` line and return the value part — but only when
/// the marker sits at the START of the line (after an optional `- ` list
/// bullet and whitespace). The writer always puts fields there; requiring it
/// stops a field VALUE containing the marker of a later field (e.g. a Reason
/// of `**Created:** 1999-…`) from hijacking that field on re-parse.
fn field_value_at_line_start<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let head = line.trim_start();
    let head = head.strip_prefix('-').map(str::trim_start).unwrap_or(head);
    head.strip_prefix(marker)
}

/// Parse a numeric score from a line like `- **Criticality:** 0.95`
pub fn parse_score_field(text: &str, field: &str) -> Option<f64> {
    let marker = format!("**{field}:**");
    let parse_after = |after: &str| -> Option<f64> {
        after
            .trim_start()
            .split(|c: char| c == '|' || c == '*' || c.is_whitespace())
            .next()?
            .trim()
            .parse()
            .ok()
    };
    // Anchored pass: field at line start (the current writer's layout).
    for line in text.lines() {
        if let Some(after) = field_value_at_line_start(line, &marker) {
            return parse_after(after);
        }
    }
    // Legacy pass: several fields on one `|`-separated line
    // (`**Criticality:** 0.95 | **Confidence:** 1.0`). Still anchored to
    // segment starts so a field VALUE elsewhere in the section (e.g. inside
    // a backticked tag) can't hijack the score.
    for line in text.lines() {
        for seg in line.split('|') {
            let seg = seg.trim();
            let seg = seg.strip_prefix('-').map(str::trim_start).unwrap_or(seg);
            if let Some(after) = seg.strip_prefix(&marker) {
                return parse_after(after);
            }
        }
    }
    None
}

/// Parse a markdown list field like `- **Files:** \`src/db/**\`, \`src/lib.rs\``
/// Returns individual items (backtick-wrapped items are unwrapped).
///
/// When the value contains backticks, items are the backtick-delimited spans —
/// splitting on every comma would corrupt items with embedded commas, e.g. the
/// glob `src/**/*.{ts,tsx}` or a tag literally containing a comma. Legacy
/// values without backticks keep the plain comma split.
pub fn parse_list_field(text: &str, field: &str) -> Vec<String> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(after) = field_value_at_line_start(line, &marker) {
            if after.contains('`') {
                let mut items = Vec::new();
                let mut rest = after;
                while let Some(start) = rest.find('`') {
                    let Some(len) = rest[start + 1..].find('`') else {
                        break;
                    };
                    let item = rest[start + 1..start + 1 + len].trim();
                    if !item.is_empty() {
                        items.push(item.to_string());
                    }
                    rest = &rest[start + 1 + len + 1..];
                }
                return items;
            }
            return after
                .split(',')
                .map(|s| s.trim().trim_matches('`').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    vec![]
}

/// Parse a datetime from a line like `- **Created:** 2026-01-15T10:00:00Z`
pub fn parse_datetime_field(text: &str, field: &str) -> Option<DateTime<Utc>> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(after) = field_value_at_line_start(line, &marker) {
            let after = after.trim();
            if let Ok(dt) = DateTime::parse_from_rfc3339(after) {
                return Some(dt.with_timezone(&Utc));
            }
            // Try chrono's more lenient parsing
            if let Ok(dt) = after.parse::<DateTime<Utc>>() {
                return Some(dt);
            }
        }
    }
    None
}

/// Parse a simple string field from `- **Key:** value` lines.
pub fn parse_string_field(text: &str, field: &str) -> Option<String> {
    let marker = format!("**{field}:**");
    for line in text.lines() {
        if let Some(after) = field_value_at_line_start(line, &marker) {
            let val = after.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Parse the ## Provenance section into a Provenance struct.
pub fn parse_provenance_section(text: &str) -> Provenance {
    let source_str = parse_string_field(text, "Source").unwrap_or_else(|| "human".to_string());
    let source = match source_str.as_str() {
        "agent" => ProvenanceSource::Agent,
        "inferred" => ProvenanceSource::Inferred,
        "imported" => ProvenanceSource::Imported,
        _ => ProvenanceSource::Human,
    };

    Provenance {
        source,
        agent_id: parse_string_field(text, "Agent"),
        model: parse_string_field(text, "Model"),
        session_id: parse_string_field(text, "Session"),
        reason: parse_string_field(text, "Reason"),
    }
}

/// Format a provenance source to its string representation.
pub fn format_provenance_source(source: ProvenanceSource) -> &'static str {
    match source {
        ProvenanceSource::Human => "human",
        ProvenanceSource::Agent => "agent",
        ProvenanceSource::Inferred => "inferred",
        ProvenanceSource::Imported => "imported",
    }
}

#[cfg(test)]
mod blockquote_tests {
    use super::*;

    // Finding #6: a `> ` blockquote line that appears *inside* a `## ` section
    // must remain section content; only a blockquote in the preamble (before any
    // section) is harvested as `__blockquote__`. Before the fix, every `> ` line
    // anywhere was stolen into `__blockquote__`, silently dropping it from the
    // section (content loss for hand-edited/legacy files).
    #[test]
    fn blockquote_inside_section_stays_in_section() {
        let body =
            "# Title\n\n> preamble quote\n\n## Content\n\n> quoted content line\nplain line\n";
        let sections = parse_body_sections(body);

        // POSITIVE: the preamble blockquote is harvested as-is.
        assert_eq!(
            sections.get("__blockquote__").map(String::as_str),
            Some("preamble quote"),
            "only the preamble blockquote should be harvested"
        );

        // NEGATIVE (red before fix): the blockquote inside ## Content must be
        // preserved as content, not stolen into __blockquote__.
        let content = sections.get("Content").map(String::as_str).unwrap_or("");
        assert!(
            content.contains("> quoted content line"),
            "blockquote inside a section was dropped; content = {content:?}"
        );
        assert!(content.contains("plain line"));
    }
}
