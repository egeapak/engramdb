//! Claude Code plugin hook handlers.
//!
//! Reads hook event JSON from stdin, retrieves relevant memories,
//! and outputs additionalContext JSON to stdout.
//!
//! Hooks fire on every Read/Write/Edit the agent performs, so latency is
//! critical. Both handlers build their engine via
//! [`engramdb::ops::build_engine_without_providers`]: hook queries carry no
//! query text (`query: None` → the engine's semantic step is skipped and
//! scoring is `scope_only`) and hooks never create memories, so embedding,
//! NLI, reranker, and T5 title models are provably never used. Skipping
//! provider resolution avoids a ~240ms ONNX session init (plus the T5
//! encoder+decoder init) on every hook invocation.

use anyhow::Result;
use engramdb::retrieval::engine::{DetailLevel, RetrievalMode, RetrievalQuery, ScoredMemory};
use engramdb::types::{Epistemic, Generality, MemoryType, Situation};
// Shared with the retrieval engine (which also relativizes `query.path`
// itself); the hook still pre-relativizes so the injected context shows
// repo-relative paths.
use engramdb::storage::paths::relativize_path;
use engramdb::storage::MemoryStore;
use std::io::Read;
use std::path::Path;

/// Extract file_path from hook input JSON.
///
/// Returns `None` if the JSON is invalid or has no `tool_input.file_path`.
fn extract_file_path(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("tool_input")
        .and_then(|ti| ti.get("file_path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// `visibility/provenance` marker for injected memory content, e.g.
/// `shared/agent` or `personal/human`.
///
/// Shared memories arrive with a `git clone` (`.engramdb/memories/` is
/// repo-adjacent), so their content is repo-authored, not necessarily the
/// local user's. Injecting them into agent context unmarked made
/// repo-shipped text indistinguishable from the user's own notes; the
/// marker lets the agent (and a reviewing human) weigh trust.
fn source_marker(m: &engramdb::types::Memory) -> String {
    format!(
        "{}/{}",
        format!("{:?}", m.visibility).to_lowercase(),
        format!("{:?}", m.provenance.source).to_lowercase()
    )
}

/// Format scored memories into a compact additionalContext string (legacy
/// flat renderer; production hooks all use the §8 class-grouped formatter).
#[cfg(test)]
fn format_additional_context(header: &str, memories: &[ScoredMemory]) -> String {
    let mut lines: Vec<String> = vec![header.into()];
    for scored in memories {
        let m = &scored.memory;
        let type_str = format!("{:?}", m.type_).to_lowercase();
        lines.push(format!(
            "- [{}] {} (criticality: {:.1}, score: {:.2}, source: {})",
            type_str,
            m.summary,
            m.criticality,
            scored.score,
            source_marker(m)
        ));
    }
    lines.join("\n")
}

/// Maximum character budget for the SessionStart additional context.
const SESSION_CONTEXT_BUDGET: usize = 2000;

/// Standing reflection nudge appended to SessionStart context.
///
/// Suggested (never required) prompt asking the agent to capture durable
/// project / environment / user-preference learnings when it finishes the
/// task it was assigned. Deliberately excludes task minutiae.
///
/// Intentionally MCP-agnostic: the SessionStart hook can run in a
/// hooks-only install with no MCP server, so this must not name MCP tools
/// or assume MCP is available. The MCP-aware variant lives in the server's
/// `instructions` string.
const REFLECTION_NUDGE: &str =
    "[EngramDB] When you finish the task you were assigned, before handing back: did anything \
durable about the project, the environment/tooling, or the user's preferences come up — not task \
minutiae? If so, review existing EngramDB memories and record the durable ones, and flag anything \
that contradicts a memory. Suggested, not required.";

/// Format scored memories with full metadata (for SessionStart).
///
/// Groups memories by type, includes tags/scope/content preview, and
/// respects a character budget. When the budget is exceeded, remaining
/// memories are omitted with a notice telling the agent to use
/// `search` for more.
#[cfg(test)] // production paths use format_class_context_with_budget directly
fn format_detailed_context(header: &str, memories: &[ScoredMemory]) -> String {
    format_detailed_context_with_budget(header, memories, SESSION_CONTEXT_BUDGET)
}

/// Build the full SessionStart additional context: project memories (if any)
/// followed by the reflection nudge. When the store has no memories the nudge
/// is still emitted on its own so the agent always receives it.
#[cfg(test)] // production path passes the config class_order via ..._with
fn build_session_start_context(memories: &[ScoredMemory]) -> String {
    build_session_start_context_with(memories, None)
}

/// [`build_session_start_context`] with a `[hooks].class_order` override.
#[cfg(test)] // production path reserves hint space via ..._reserving
fn build_session_start_context_with(
    memories: &[ScoredMemory],
    class_order: Option<&[String]>,
) -> String {
    build_session_start_context_reserving(memories, class_order, 0)
}

/// [`build_session_start_context_with`], reserving `reserved` chars of the
/// §14.9 budget for a trailing line the caller will append (the §16.4 hint
/// counts against the 2000-char budget, so the memory body must shrink to
/// make room for it).
fn build_session_start_context_reserving(
    memories: &[ScoredMemory],
    class_order: Option<&[String]>,
    reserved: usize,
) -> String {
    if memories.is_empty() {
        REFLECTION_NUDGE.to_string()
    } else {
        format!(
            "{}\n\n{}",
            format_class_context_with_budget(
                "[EngramDB] Key project memories:",
                memories,
                SESSION_CONTEXT_BUDGET.saturating_sub(reserved),
                Some(Situation::SessionStart),
                class_order,
            ),
            REFLECTION_NUDGE
        )
    }
}

/// Class-order rank for a scored memory under a situation (§8.1).
///
/// `class_order` (from `[hooks].class_order`) overrides the per-situation
/// defaults with one uniform ordering. Unknown class names in the override
/// sink to the end.
fn class_rank(
    class: Epistemic,
    situation: Option<Situation>,
    class_order: Option<&[String]>,
) -> usize {
    if let Some(order) = class_order {
        return order
            .iter()
            .position(|c| c.eq_ignore_ascii_case(class.as_str()))
            .unwrap_or(order.len());
    }
    match situation {
        // FileEdit: decisions bind on the edit; facts next; observations last.
        Some(Situation::FileEdit) => match class {
            Epistemic::Decision => 0,
            Epistemic::Fact => 1,
            Epistemic::Observation => 2,
        },
        // SessionStart (and default): facts → decisions → observations.
        _ => match class {
            Epistemic::Fact => 0,
            Epistemic::Decision => 1,
            Epistemic::Observation => 2,
        },
    }
}

/// Group scored memories by epistemic class in situation order (§8.1),
/// preserving score order within each group — except FileEdit's fact group,
/// where hazard-typed entries sort first (a deliberate new ordering: the
/// footgun outranks the description when about to edit).
fn group_by_class<'a>(
    memories: &'a [ScoredMemory],
    situation: Option<Situation>,
    class_order: Option<&[String]>,
) -> Vec<(Epistemic, Vec<&'a ScoredMemory>)> {
    let mut groups: Vec<(Epistemic, Vec<&ScoredMemory>)> = Vec::new();
    for class in [Epistemic::Fact, Epistemic::Observation, Epistemic::Decision] {
        let mut group: Vec<&ScoredMemory> = memories
            .iter()
            .filter(|sm| sm.memory.epistemic == class)
            .collect();
        if group.is_empty() {
            continue;
        }
        if situation == Some(Situation::FileEdit) && class == Epistemic::Fact {
            // Stable sort: hazards first, otherwise keep score order.
            group.sort_by_key(|sm| (sm.memory.type_ != MemoryType::Hazard) as u8);
        }
        groups.push((class, group));
    }
    groups.sort_by_key(|(class, _)| class_rank(*class, situation, class_order));
    groups
}

/// Human-facing plural header for a class group.
fn class_header(class: Epistemic) -> &'static str {
    match class {
        Epistemic::Fact => "Facts",
        Epistemic::Observation => "Observations",
        Epistemic::Decision => "Decisions",
    }
}

/// Per-class rendering (§8.2). Returns the entry lines; `compact` drops the
/// fact preview (the budget policy's first compression step).
///
/// - Decision: `- {summary} — because {premise}[; revisit if {globs}]`;
///   summary only when no premise (never invent a rationale).
/// - Observation: `- {summary} (observed date[, verified date])`.
/// - Fact: compact one-liner, `(verified date)` only when set; optional
///   content preview line when not compacting.
///
/// Every line carries the `source: visibility/provenance` marker — shared
/// memories arrive with a git clone, and injected context must keep
/// repo-shipped text distinguishable from the user's own notes.
fn format_class_entry(scored: &ScoredMemory, compact: bool) -> Vec<String> {
    let m = &scored.memory;
    let src = source_marker(m);
    let type_str = format!("{:?}", m.type_).to_lowercase();
    match m.epistemic {
        Epistemic::Decision => {
            let mut line = format!("- [{}] {}", type_str, m.summary);
            if let Some(validity) = &m.valid_while {
                if let Some(premise) = &validity.premise {
                    line.push_str(&format!(" — because {}", premise));
                }
                if !validity.invalidated_by.is_empty() {
                    line.push_str(&format!(
                        "; revisit if {} changes",
                        validity.invalidated_by.join(", ")
                    ));
                }
            }
            line.push_str(&format!(" (source: {})", src));
            vec![line]
        }
        Epistemic::Observation => {
            let mut line = format!(
                "- [{}] {} (observed {}",
                type_str,
                m.summary,
                m.created_at.format("%Y-%m-%d")
            );
            if let Some(v) = m.verified_at {
                line.push_str(&format!(", verified {}", v.format("%Y-%m-%d")));
            }
            line.push_str(&format!("; source: {})", src));
            vec![line]
        }
        Epistemic::Fact => {
            let mut line = format!("- [{}] {}", type_str, m.summary);
            if let Some(v) = m.verified_at {
                line.push_str(&format!(" (verified {})", v.format("%Y-%m-%d")));
            }
            line.push_str(&format!(" (source: {})", src));
            let mut entry = vec![line];
            if !compact {
                let preview = truncate_content(&m.content, 200);
                if preview != m.summary {
                    entry.push(format!("  {}", preview));
                }
            }
            entry
        }
    }
}

/// Suppress task-scoped memories from hook injection (§8.3): entries with
/// `generality == Task` are dropped unless the session's declared task
/// (§11.1 mapping) matches their `origin_task`. Absent a mapping,
/// task-scoped memories are suppressed from hooks but remain reachable by
/// explicit query.
///
/// Recorded deviation from §8.3: suppression runs on the MATERIALIZED
/// result (after the engine's `max_results` truncation), not at the index
/// level. Consequence: a top-k dominated by foreign task-scoped memories
/// yields fewer injected entries rather than backfilling project-wide ones.
/// Accepted because hooks request small k (5/10) against the same index the
/// engine already filtered, and the §16.4 hint tells the agent exactly how
/// to recover the hidden ones.
fn suppress_task_scoped(
    memories: Vec<ScoredMemory>,
    current_task: Option<&str>,
) -> Vec<ScoredMemory> {
    memories
        .into_iter()
        .filter(|sm| {
            sm.memory.valid_while.as_ref().is_none_or(|v| {
                v.generality != Generality::Task
                    || (current_task.is_some() && v.origin_task.as_deref() == current_task)
            })
        })
        .collect()
}

/// The session's declared task, resolved from the hook event's session id
/// and the §11.1 mapping. Falls back to the freshest mapping from ANY
/// session when this session id has no entry: the MCP `task_current` tool
/// records the mapping under the server process's own session id, which
/// never matches the id in a hook event, so without the fallback the
/// hook-taught "declare task_current to surface yours" flow would be a
/// no-op under the default plugin install. `None` when the event carries no
/// session id or nothing fresh is mapped.
fn session_task_for(input: &str, dir: &Path) -> Option<String> {
    let session_id = extract_session_id(input)?;
    engramdb::storage::task_state::current_task_or_recent(dir, &session_id)
}

/// Budget-aware implementation (extracted for testability).
///
/// Budget policy (§8.4): decisions are ATOMIC — if the whole line (with its
/// because-clause) doesn't fit, the entry is skipped entirely, never
/// truncated mid-rationale. Facts are compressible — the preview line is
/// dropped first, then the summary line competes like any other. Observations
/// are a single summary+date line.
#[cfg(test)] // budget-parameterized entry point for the formatter tests
fn format_detailed_context_with_budget(
    header: &str,
    memories: &[ScoredMemory],
    budget: usize,
) -> String {
    format_class_context_with_budget(header, memories, budget, None, None)
}

/// Class-grouped, situation-ordered, budget-aware context formatter (§8).
fn format_class_context_with_budget(
    header: &str,
    memories: &[ScoredMemory],
    budget: usize,
    situation: Option<Situation>,
    class_order: Option<&[String]>,
) -> String {
    let groups = group_by_class(memories, situation, class_order);

    let mut lines: Vec<String> = vec![header.into()];
    let mut used: usize = header.len();
    let mut included = 0usize;
    let total = memories.len();

    // Reserve room for the worst-case omission notice up front so appending
    // it can never push the output over the §14.9 cap. Only at realistic
    // budgets — at tiny (test/degenerate) budgets the reserve would crowd
    // out the content itself, and the cap those budgets model isn't real.
    const OMITTED_RESERVE: usize = "\n(999 more memories omitted — use query to find them)".len();
    let body_budget = if budget >= 4 * OMITTED_RESERVE {
        budget - OMITTED_RESERVE
    } else {
        budget
    };

    for (class, group) in &groups {
        let group_header = format!("\n## {} ({}):", class_header(*class), group.len());
        let group_header_len = group_header.len() + 1; // +1 for join newline

        if used + group_header_len > body_budget {
            break;
        }

        // Emit the header only once an entry from this group actually fits —
        // a header with zero surviving entries is noise, not context.
        let mut header_emitted = false;

        for scored in group {
            let entry = format_class_entry(scored, false);
            let entry_len: usize = entry.iter().map(|l| l.len() + 1).sum();
            let pending_header = if header_emitted { 0 } else { group_header_len };

            if used + pending_header + entry_len <= body_budget {
                if !header_emitted {
                    lines.push(group_header.clone());
                    used += group_header_len;
                    header_emitted = true;
                }
                lines.extend(entry);
                used += entry_len;
                included += 1;
                continue;
            }

            // Over budget: facts compress (drop the preview line first);
            // decisions and observations are atomic and are skipped whole.
            if *class == Epistemic::Fact {
                let compact_entry = format_class_entry(scored, true);
                let compact_len: usize = compact_entry.iter().map(|l| l.len() + 1).sum();
                if used + pending_header + compact_len <= body_budget {
                    if !header_emitted {
                        lines.push(group_header.clone());
                        used += group_header_len;
                        header_emitted = true;
                    }
                    lines.extend(compact_entry);
                    used += compact_len;
                    included += 1;
                }
            }
        }
    }

    if included < total {
        let omitted = total - included;
        lines.push(format!(
            "\n({} more memories omitted — use query to find them)",
            omitted
        ));
    }

    lines.join("\n")
}

/// Truncate content to a maximum character length, appending "..." if truncated.
fn truncate_content(content: &str, max_chars: usize) -> String {
    let single_line = content.replace('\n', " ");
    if single_line.len() <= max_chars {
        single_line
    } else {
        let truncated: String = single_line.chars().take(max_chars).collect();
        format!("{}...", truncated.trim_end())
    }
}

/// Build the hook response JSON string.
fn build_hook_response(event_name: &str, additional_context: &str) -> Result<String> {
    let response = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": additional_context
        }
    });
    Ok(serde_json::to_string(&response)?)
}

/// Core hook logic: given input JSON, project dir, and store, retrieve and format.
///
/// Returns `Ok(Some(json))` if memories were found, `Ok(None)` if nothing to output.
async fn process_hook_input(input: &str, dir: &Path, store: MemoryStore) -> Result<Option<String>> {
    let file_path = match extract_file_path(input) {
        Some(fp) => fp,
        None => return Ok(None),
    };

    let relative_path = relativize_path(&file_path, dir);

    let config_path = dir.join(".engramdb").join("config.toml");
    // No model providers: the query below has `query: None`, so retrieval is
    // scope_only and never embeds (see module docs).
    let engine = engramdb::ops::build_engine_without_providers(store, &config_path).await;

    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        path: Some(relative_path),
        logical: vec![],
        query: None,
        types: None,
        tags: None,
        min_criticality: None,
        max_results: Some(5),
        include_expired: Some(false),
        detail_level: DetailLevel::Summary,
        situation: Some(Situation::FileEdit),
        ..Default::default()
    };

    let result = match engramdb::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    // Task-scoped memories are suppressed from hook injection (§8.3) unless
    // the session declared their task, and the surviving list is ordered by
    // FileEdit class rank (§8.1: decisions first, then facts — hazards
    // leading — then observations).
    let current_task = session_task_for(input, dir);
    let mut memories = suppress_task_scoped(result.memories, current_task.as_deref());
    let class_order = engine.config().hooks.class_order.clone();
    memories.sort_by_key(|sm| {
        (
            class_rank(
                sm.memory.epistemic,
                Some(Situation::FileEdit),
                class_order.as_deref(),
            ),
            (sm.memory.epistemic == Epistemic::Fact && sm.memory.type_ != MemoryType::Hazard) as u8,
        )
    });
    if memories.is_empty() {
        return Ok(None);
    }

    // §8.1/§8.2 class-grouped rendering (same formatter as SessionStart /
    // UserPromptSubmit): decisions carry their "— because {premise}" clause
    // atomically, facts show "revisit if" globs, groups get class headers.
    // The pre-sort above still matters — group_by_class is stable, so the
    // hazard-first ordering inside the facts group survives.
    let budget = engine.config().hooks.prompt_context_budget;
    let context = format_class_context_with_budget(
        "[EngramDB] Relevant memories for this file:",
        &memories,
        budget,
        Some(Situation::FileEdit),
        class_order.as_deref(),
    );
    let json = build_hook_response("PreToolUse", &context)?;
    Ok(Some(json))
}

/// Run the PreToolUse hook handler.
///
/// Reads JSON from stdin, extracts `tool_input.file_path`,
/// retrieves relevant memories for that path, and prints
/// JSON with `additionalContext` to stdout.
pub async fn run_hook_pre_tool_use(dir: &Path) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    // Open store — if it fails, exit silently (store may not be initialized)
    let store = match MemoryStore::open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(());
        }
    };

    if let Some(json) = process_hook_input(&input, dir, store).await? {
        println!("{}", json);
    }

    Ok(())
}

/// Core SessionStart logic: returns the JSON to print, or `None` if nothing
/// should be surfaced (store not initialized, no memories above threshold,
/// retrieval failed). Split from [`run_hook_session_start`] so the body is
/// unit-testable without stdout capture — mirrors how `process_hook_input`
/// backs `run_hook_pre_tool_use`.
async fn process_session_start(
    dir: &Path,
    min_criticality: f64,
    input: &str,
) -> Result<Option<String>> {
    let store = match MemoryStore::open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    let config_path = dir.join(".engramdb").join("config.toml");
    // No model providers: the query below has `query: None`, so retrieval is
    // scope_only and never embeds (see module docs).
    let engine = engramdb::ops::build_engine_without_providers(store, &config_path).await;

    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        path: None,
        logical: vec![],
        query: None,
        types: None,
        tags: None,
        min_criticality: Some(min_criticality),
        max_results: Some(10),
        include_expired: Some(false),
        detail_level: DetailLevel::Summary,
        situation: Some(Situation::SessionStart),
        ..Default::default()
    };

    let result = match engramdb::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    // Always emit the reflection nudge, even when the store has no memories
    // yet — the agent should still be reminded to capture durable learnings.
    let current_task = session_task_for(input, dir);
    let before = result.memories.len();
    let memories = suppress_task_scoped(result.memories, current_task.as_deref());
    let suppressed = before - memories.len();
    let class_order = engine.config().hooks.class_order.clone();
    // §16.4 hint line: teach the task mechanism exactly when it becomes
    // relevant. Safe to advertise `task_current` — the tool ships in this
    // build (it is pinned into MCP_TOOL_SUFFIXES alongside this hint). Built
    // first so its length is reserved out of the 2000-char budget (§14.9).
    let hint = (suppressed > 0).then(|| {
        format!(
            "\n({suppressed} task-scoped memories hidden — declare task_current to surface yours.)"
        )
    });
    let reserved = hint.as_ref().map(|h| h.len()).unwrap_or(0);
    let mut context =
        build_session_start_context_reserving(&memories, class_order.as_deref(), reserved);
    if let Some(hint) = hint {
        context.push_str(&hint);
    }
    let json = build_hook_response("SessionStart", &context)?;
    Ok(Some(json))
}

/// Run the SessionStart hook handler.
///
/// Retrieves high-criticality active memories and prints them as
/// additionalContext JSON to stdout so they are surfaced at session start.
pub async fn run_hook_session_start(dir: &Path, min_criticality: f64) -> Result<()> {
    let min_criticality = sanitize_min_criticality(min_criticality);
    // The event JSON carries the session id used for §8.3 task
    // un-suppression; a missing/closed stdin degrades to no session id.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Some(json) = process_session_start(dir, min_criticality, &input).await? {
        println!("{}", json);
    }
    Ok(())
}

/// Bound the SessionStart criticality threshold into `[0, 1]`, mapping NaN to
/// the default 0.6. The hook is machine-invoked and must stay robust (an
/// out-of-range or NaN value otherwise filtered out every memory or made every
/// comparison false), so we sanitize rather than error (finding #21).
fn sanitize_min_criticality(v: f64) -> f64 {
    if v.is_nan() {
        0.6
    } else {
        v.clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// New hook events (§8.5)
// ---------------------------------------------------------------------------

/// Situation-inference keyword tables for UserPromptSubmit (§8.5.1). Cheap
/// substring heuristics — the only place Debugging/DesignChoice can be
/// inferred automatically.
const DEBUGGING_KEYWORDS: &[&str] = &["error", "failing", "why does", "debug", "panic", "crash"];
const DESIGN_KEYWORDS: &[&str] = &[
    "should we",
    "choose",
    " vs ",
    "design",
    "approach",
    "architecture",
];

/// Infer a situation from the submitted prompt text (`None` when no keyword
/// table matches — neutral scoring).
fn infer_situation(prompt: &str) -> Option<Situation> {
    let lower = prompt.to_lowercase();
    if DEBUGGING_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return Some(Situation::Debugging);
    }
    if DESIGN_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return Some(Situation::DesignChoice);
    }
    None
}

/// Extract the `session_id` field every hook event carries.
fn extract_session_id(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// Extract the submitted prompt text from a UserPromptSubmit event.
fn extract_prompt(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    value
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
}

/// Core UserPromptSubmit logic (§8.5.1): Filter-mode query with the prompt as
/// query text plus inferred situation; injects top-k under
/// `[hooks].prompt_context_budget` with per-class rendering. Keyword-only
/// retrieval — hooks never load model providers.
async fn process_user_prompt_submit(input: &str, dir: &Path) -> Result<Option<String>> {
    let prompt = match extract_prompt(input) {
        Some(p) => p,
        None => return Ok(None),
    };

    let store = match MemoryStore::open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(None);
        }
    };
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = engramdb::ops::build_engine_without_providers(store, &config_path).await;

    let situation = infer_situation(&prompt);
    let query = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some(prompt),
        max_results: Some(5),
        include_expired: Some(false),
        detail_level: DetailLevel::Summary,
        situation,
        ..Default::default()
    };

    let result = match engramdb::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    let current_task = session_task_for(input, dir);
    let memories = suppress_task_scoped(result.memories, current_task.as_deref());
    if memories.is_empty() {
        return Ok(None);
    }

    let budget = engine.config().hooks.prompt_context_budget;
    let class_order = engine.config().hooks.class_order.clone();
    let context = format_class_context_with_budget(
        "[EngramDB] Memories relevant to this prompt:",
        &memories,
        budget,
        situation,
        class_order.as_deref(),
    );
    let json = build_hook_response("UserPromptSubmit", &context)?;
    Ok(Some(json))
}

/// Run the UserPromptSubmit hook handler. Malformed/empty stdin ⇒ exit 0
/// with empty stdout, never an error.
pub async fn run_hook_user_prompt_submit(dir: &Path) -> Result<()> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Ok(Some(json)) = process_user_prompt_submit(&input, dir).await {
        println!("{}", json);
    }
    Ok(())
}

/// Core PostToolUse logic (§8.5.2): after a file mutation, match the edited
/// path against the `watch_paths` index column, restricted to currently-valid
/// memories (this hook bypasses the query path, so it applies the §2.4
/// default exclusion itself — an invalidated memory must not keep warning).
/// Index-only: no memory files are loaded.
async fn process_post_tool_use(input: &str, dir: &Path) -> Result<Option<String>> {
    let file_path = match extract_file_path(input) {
        Some(fp) => fp,
        None => return Ok(None),
    };
    let relative_path = relativize_path(&file_path, dir);
    if relative_path.is_empty() {
        return Ok(None);
    }

    let store = match MemoryStore::open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    let entries = match store.list_for_filtering().await {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("Hook index read failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    let now = chrono::Utc::now();
    let matched_ids: Vec<&str> = entries
        .iter()
        .filter(|entry| {
            // §2.4 validity guard: closed windows never warn (§14.14).
            entry.invalidated_at.is_none_or(|t| t > now)
                && !entry.watch_paths.is_empty()
                && engramdb::scope::physical::matches(&entry.watch_paths, &relative_path)
        })
        .map(|entry| entry.id.as_str())
        .collect();
    if matched_ids.is_empty() {
        // The common case: one index scan, zero file loads, no output.
        return Ok(None);
    }

    // Load ONLY the matched memories (rare, bounded) for their summaries.
    let matched = store.get_batch(&matched_ids).await.unwrap_or_default();
    let warnings: Vec<String> = matched
        .iter()
        .map(|(id, m)| {
            format!(
                "⚠ this edit may invalidate memory {} ('{}') — verify it or update/invalidate it",
                crate::output::short_id(id),
                m.summary
            )
        })
        .collect();
    if warnings.is_empty() {
        return Ok(None);
    }
    let context = format!("[EngramDB]\n{}", warnings.join("\n"));
    let json = build_hook_response("PostToolUse", &context)?;
    Ok(Some(json))
}

/// Run the PostToolUse hook handler (matcher `Write|Edit|MultiEdit`).
pub async fn run_hook_post_tool_use(dir: &Path) -> Result<()> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Ok(Some(json)) = process_post_tool_use(&input, dir).await {
        println!("{}", json);
    }
    Ok(())
}

/// Run the SessionEnd hook handler (§8.5.3): housekeeping only, no context
/// output, must never block session teardown. Clears the session→task
/// mapping (§11.1) and, when `[epistemic].demote_on_session_end` is set and
/// a mapping existed, runs the §11.2 demotion for that task. Best-effort:
/// every failure is logged and swallowed. (Telemetry flushes live in the
/// long-running MCP/daemon processes; a one-shot hook has none.)
pub async fn run_hook_session_end(dir: &Path) -> Result<()> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);

    let Some(session_id) = extract_session_id(&input) else {
        return Ok(());
    };
    let ended_task = match engramdb::storage::task_state::clear_session_task(dir, &session_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!("SessionEnd mapping clear failed (non-fatal): {e}");
            None
        }
    };

    if let Some(task) = ended_task {
        let config_path = dir.join(".engramdb").join("config.toml");
        let config = engramdb::storage::config::load_config_or_default(&config_path).await;
        if config.epistemic.demote_on_session_end {
            match MemoryStore::open(dir).await {
                Ok(store) => {
                    if let Err(e) =
                        engramdb::ops::task_complete(&store, &task, &config.epistemic).await
                    {
                        tracing::debug!("SessionEnd demotion failed (non-fatal): {e}");
                    }
                }
                Err(e) => tracing::debug!("SessionEnd store open failed (non-fatal): {e}"),
            }
        }
    }
    Ok(())
}

/// Static PreCompact reminder (§8.5.4): context loss is a memory system's
/// worst enemy; intercept right before compaction.
const PRE_COMPACT_REMINDER: &str = "[EngramDB] Context is about to be compacted. Store durable \
discoveries first: decisions (with their premise), hazards, and verified observations via the \
memory create tool.";

/// Run the PreCompact hook handler (§8.5.4): inject a short static reminder.
/// Same additionalContext contract as the other hooks; if the runtime
/// ignores it for PreCompact events, the output is harmlessly dropped.
pub async fn run_hook_pre_compact(_dir: &Path) -> Result<()> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let json = build_hook_response("PreCompact", PRE_COMPACT_REMINDER)?;
    println!("{}", json);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::storage::{InMemoryRegistry, MemoryStore};

    // Finding #21: the SessionStart criticality threshold must be sanitized.
    #[test]
    fn sanitize_min_criticality_bounds_and_handles_nan() {
        // POSITIVE: in-range values pass through.
        assert_eq!(sanitize_min_criticality(0.0), 0.0);
        assert_eq!(sanitize_min_criticality(0.6), 0.6);
        assert_eq!(sanitize_min_criticality(1.0), 1.0);
        // NEGATIVE (red before fix): out-of-range clamps, NaN → default 0.6.
        assert_eq!(sanitize_min_criticality(5.0), 1.0);
        assert_eq!(sanitize_min_criticality(-1.0), 0.0);
        assert_eq!(sanitize_min_criticality(f64::NAN), 0.6);
    }
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    // --- Unit tests for extract_file_path ---

    #[test]
    fn test_extract_file_path_from_read_tool() {
        let input = r#"{"tool_name":"Read","tool_input":{"file_path":"/project/src/main.rs"}}"#;
        assert_eq!(
            extract_file_path(input),
            Some("/project/src/main.rs".to_string())
        );
    }

    #[test]
    fn test_extract_file_path_from_write_tool() {
        let input =
            r#"{"tool_name":"Write","tool_input":{"file_path":"/project/out.txt","content":"hi"}}"#;
        assert_eq!(
            extract_file_path(input),
            Some("/project/out.txt".to_string())
        );
    }

    #[test]
    fn test_extract_file_path_from_edit_tool() {
        let input = r#"{"tool_name":"Edit","tool_input":{"file_path":"/project/lib.rs","old_string":"a","new_string":"b"}}"#;
        assert_eq!(
            extract_file_path(input),
            Some("/project/lib.rs".to_string())
        );
    }

    #[test]
    fn test_extract_file_path_missing_tool_input() {
        let input = r#"{"tool_name":"Bash"}"#;
        assert_eq!(extract_file_path(input), None);
    }

    #[test]
    fn test_extract_file_path_no_file_path_field() {
        let input = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        assert_eq!(extract_file_path(input), None);
    }

    #[test]
    fn test_extract_file_path_invalid_json() {
        assert_eq!(extract_file_path("not json at all"), None);
    }

    #[test]
    fn test_extract_file_path_empty_string() {
        assert_eq!(extract_file_path(""), None);
    }

    #[test]
    fn test_extract_file_path_numeric_value() {
        let input = r#"{"tool_input":{"file_path":42}}"#;
        assert_eq!(extract_file_path(input), None);
    }

    // --- Unit tests for relativize_path ---

    #[test]
    fn test_relativize_path_inside_project() {
        let dir = Path::new("/Users/test/project");
        assert_eq!(
            relativize_path("/Users/test/project/src/main.rs", dir),
            "src/main.rs"
        );
    }

    #[test]
    fn test_relativize_path_outside_project() {
        let dir = Path::new("/Users/test/project");
        assert_eq!(
            relativize_path("/Users/other/file.rs", dir),
            "/Users/other/file.rs"
        );
    }

    #[test]
    fn test_relativize_path_project_root_itself() {
        let dir = Path::new("/Users/test/project");
        assert_eq!(relativize_path("/Users/test/project", dir), "");
    }

    #[test]
    fn test_relativize_path_already_relative() {
        let dir = Path::new("/Users/test/project");
        assert_eq!(relativize_path("src/main.rs", dir), "src/main.rs");
    }

    #[test]
    fn test_relativize_path_dot_dir_with_absolute_file() {
        // Simulates `--dir .` with an absolute file path from tool input.
        // Uses a real temp directory so canonicalize succeeds.
        let temp_dir = TempDir::new().unwrap();
        let sub = temp_dir.path().join("src").join("cli");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("add.rs"), "").unwrap();

        let canonical = temp_dir.path().canonicalize().unwrap();
        let abs_file = canonical.join("src/cli/add.rs");

        let result = relativize_path(abs_file.to_str().unwrap(), temp_dir.path());
        assert_eq!(result, "src/cli/add.rs");
    }

    #[test]
    fn test_relativize_path_nonexistent_file_falls_back() {
        // When the file doesn't exist on disk, canonicalize fails and
        // we fall back to raw strip_prefix. If that also fails, the
        // original path is returned unchanged.
        let result = relativize_path(
            "/nonexistent/project/src/main.rs",
            Path::new("/nonexistent/project"),
        );
        assert_eq!(result, "src/main.rs");
    }

    #[test]
    fn test_relativize_path_file_at_project_root() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("Cargo.toml"), "").unwrap();

        let canonical = temp_dir.path().canonicalize().unwrap();
        let abs_file = canonical.join("Cargo.toml");

        let result = relativize_path(abs_file.to_str().unwrap(), temp_dir.path());
        assert_eq!(result, "Cargo.toml");
    }

    #[test]
    fn test_relativize_path_deeply_nested() {
        let temp_dir = TempDir::new().unwrap();
        let deep = temp_dir.path().join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("file.rs"), "").unwrap();

        let canonical = temp_dir.path().canonicalize().unwrap();
        let abs_file = canonical.join("a/b/c/d/file.rs");

        let result = relativize_path(abs_file.to_str().unwrap(), temp_dir.path());
        assert_eq!(result, "a/b/c/d/file.rs");
    }

    // --- Unit tests for format_additional_context ---

    #[test]
    fn test_format_additional_context_single_memory() {
        let mem = Memory::new(
            MemoryType::Decision,
            "Use snake_case everywhere",
            "Convention for naming",
            Provenance::human(),
        );
        let scored = ScoredMemory {
            memory: mem,
            score: 0.85,
            score_breakdown: Default::default(),
        };
        let ctx =
            format_additional_context("[EngramDB] Relevant memories for this file:", &[scored]);
        assert!(ctx.starts_with("[EngramDB] Relevant memories for this file:"));
        assert!(ctx.contains("[decision]"));
        assert!(ctx.contains("Use snake_case everywhere"));
        assert!(ctx.contains("score: 0.85"));
    }

    #[test]
    fn test_format_additional_context_multiple_memories() {
        let mem1 = Memory::new(
            MemoryType::Hazard,
            "Do not delete index",
            "Content 1",
            Provenance::human(),
        );
        let mem2 = Memory::new(
            MemoryType::Convention,
            "Always run clippy",
            "Content 2",
            Provenance::human(),
        );
        let scored = vec![
            ScoredMemory {
                memory: mem1,
                score: 0.9,
                score_breakdown: Default::default(),
            },
            ScoredMemory {
                memory: mem2,
                score: 0.7,
                score_breakdown: Default::default(),
            },
        ];
        let ctx = format_additional_context("[EngramDB] Relevant memories for this file:", &scored);
        let lines: Vec<&str> = ctx.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 memories
        assert!(lines[1].contains("[hazard]"));
        assert!(lines[2].contains("[convention]"));
    }

    #[test]
    fn test_format_additional_context_empty() {
        let ctx = format_additional_context("[EngramDB] Relevant memories for this file:", &[]);
        assert_eq!(ctx, "[EngramDB] Relevant memories for this file:");
    }

    #[test]
    fn test_format_additional_context_custom_header() {
        let ctx = format_additional_context("[EngramDB] Key project memories:", &[]);
        assert_eq!(ctx, "[EngramDB] Key project memories:");
    }

    // --- Unit tests for format_detailed_context (grouped + budget) ---

    #[test]
    fn test_format_detailed_context_fact_rendering() {
        // Convention defaults to the Fact class: compact line, type tag,
        // content preview when it adds information, source marker.
        let mut mem = Memory::new(
            MemoryType::Convention,
            "Azure DevOps PR conventions",
            "PR templates are stored in .azuredevops/pull_request_template/",
            Provenance::human(),
        );
        mem.criticality = 0.8;
        let scored = ScoredMemory {
            memory: mem,
            score: 0.9,
            score_breakdown: Default::default(),
        };
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &[scored]);
        assert!(ctx.contains("## Facts (1):"), "{ctx}");
        assert!(ctx.contains("[convention] Azure DevOps PR conventions"));
        assert!(ctx.contains("PR templates are stored in"));
        assert!(ctx.contains("source: shared/human"));
        assert!(
            !ctx.contains("(verified"),
            "no verified_at ⇒ no verified tag"
        );
    }

    /// Injected memory content must carry a `visibility/provenance` marker:
    /// shared memories arrive with a `git clone`, so the agent (and a
    /// reviewing human) must be able to tell repo-shipped content from the
    /// user's own personal notes.
    #[test]
    fn test_injected_context_marks_visibility_and_provenance() {
        let mut shared = Memory::new(
            MemoryType::Convention,
            "Repo-shipped convention",
            "Content that arrived with the clone",
            Provenance::agent("mcp"),
        );
        shared.visibility = engramdb::types::Visibility::Shared;
        let mut personal = Memory::new(
            MemoryType::Preference,
            "My own preference",
            "Local-only note",
            Provenance::human(),
        );
        personal.visibility = engramdb::types::Visibility::Personal;
        let scored: Vec<ScoredMemory> = [shared, personal]
            .into_iter()
            .map(|memory| ScoredMemory {
                memory,
                score: 0.9,
                score_breakdown: Default::default(),
            })
            .collect();

        // Detailed (SessionStart) format.
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &scored);
        assert!(ctx.contains("source: shared/agent"), "{ctx}");
        assert!(ctx.contains("source: personal/human"), "{ctx}");

        // Compact (PreToolUse) format.
        let ctx = format_additional_context("[EngramDB]", &scored);
        assert!(ctx.contains("source: shared/agent"), "{ctx}");
        assert!(ctx.contains("source: personal/human"), "{ctx}");
    }

    #[test]
    fn test_format_detailed_context_groups_by_class_in_session_order() {
        // SessionStart default order: Facts → Decisions → Observations.
        let fact = Memory::new(
            MemoryType::Hazard,
            "Do not delete index",
            "Index deletion causes data loss",
            Provenance::human(),
        );
        let decision = Memory::new(
            MemoryType::Decision,
            "Use rustls",
            "We chose rustls over openssl",
            Provenance::human(),
        );
        let observation = Memory::new(
            MemoryType::Debug,
            "Tests flake under load",
            "Two tests fail under full parallelism",
            Provenance::human(),
        );
        let scored: Vec<ScoredMemory> = [observation, decision, fact]
            .into_iter()
            .map(|memory| ScoredMemory {
                memory,
                score: 0.9,
                score_breakdown: Default::default(),
            })
            .collect();
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &scored);
        let facts_pos = ctx.find("## Facts").unwrap();
        let decisions_pos = ctx.find("## Decisions").unwrap();
        let observations_pos = ctx.find("## Observations").unwrap();
        assert!(facts_pos < decisions_pos, "{ctx}");
        assert!(decisions_pos < observations_pos, "{ctx}");
    }

    #[test]
    fn test_format_detailed_context_budget_truncation() {
        // Create enough memories to exceed a small budget
        let memories: Vec<ScoredMemory> = (0..10)
            .map(|i| {
                let mem = Memory::new(
                    MemoryType::Decision,
                    format!("Decision number {}", i),
                    format!("Detailed content for decision {}", i),
                    Provenance::human(),
                );
                ScoredMemory {
                    memory: mem,
                    score: 0.9 - (i as f64 * 0.05),
                    score_breakdown: Default::default(),
                }
            })
            .collect();
        // Use a small budget so not all fit
        let ctx =
            format_detailed_context_with_budget("[EngramDB] Key project memories:", &memories, 500);
        assert!(ctx.contains("more memories omitted"));
        assert!(ctx.contains("use query to find them"));
        // Should include at least 1 but not all 10
        let included = memories
            .iter()
            .filter(|m| ctx.contains(&m.memory.summary))
            .count();
        assert!(included >= 1);
        assert!(included < 10);
    }

    #[test]
    fn test_format_detailed_context_no_truncation_notice_when_all_fit() {
        let mem = Memory::new(
            MemoryType::Decision,
            "Use async everywhere",
            "All I/O should be async",
            Provenance::human(),
        );
        let scored = ScoredMemory {
            memory: mem,
            score: 0.5,
            score_breakdown: Default::default(),
        };
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &[scored]);
        assert!(!ctx.contains("omitted"));
    }

    #[test]
    fn test_format_detailed_context_skips_fact_preview_matching_summary() {
        // Context type defaults to the Fact class; a preview identical to
        // the summary adds nothing and is dropped.
        let mem = Memory::new(
            MemoryType::Context,
            "Use async everywhere",
            "Use async everywhere",
            Provenance::human(),
        );
        let scored = ScoredMemory {
            memory: mem,
            score: 0.5,
            score_breakdown: Default::default(),
        };
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &[scored]);
        let content_lines: Vec<&str> = ctx.lines().filter(|l| l.starts_with("  ")).collect();
        assert!(content_lines.is_empty());
    }

    #[test]
    fn test_format_detailed_context_minimal_fact_line() {
        let mem = Memory::new(
            MemoryType::Hazard,
            "Avoid blocking in async",
            "Blocking calls in async context cause deadlocks",
            Provenance::human(),
        );
        let scored = ScoredMemory {
            memory: mem,
            score: 0.7,
            score_breakdown: Default::default(),
        };
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &[scored]);
        assert!(ctx.contains("[hazard] Avoid blocking in async"));
        assert!(ctx.contains("source: shared/human"));
    }

    // --- Unit tests for group_by_class (§8.1) ---

    #[test]
    fn test_group_by_class_orders_per_situation() {
        let fact = Memory::new(MemoryType::Convention, "Conv 1", "c1", Provenance::human());
        let hazard_fact = Memory::new(MemoryType::Hazard, "Hazard 1", "h1", Provenance::human());
        let decision = Memory::new(MemoryType::Decision, "Dec 1", "d1", Provenance::human());
        let scored: Vec<ScoredMemory> = [fact, hazard_fact, decision]
            .into_iter()
            .map(|memory| ScoredMemory {
                memory,
                score: 0.9,
                score_breakdown: Default::default(),
            })
            .collect();

        // SessionStart: facts (both types) first, decisions after.
        let groups = group_by_class(&scored, Some(Situation::SessionStart), None);
        assert_eq!(groups[0].0, Epistemic::Fact);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, Epistemic::Decision);

        // FileEdit: decisions first; hazard-typed facts lead the fact group.
        let groups = group_by_class(&scored, Some(Situation::FileEdit), None);
        assert_eq!(groups[0].0, Epistemic::Decision);
        assert_eq!(groups[1].0, Epistemic::Fact);
        assert_eq!(groups[1].1[0].memory.type_, MemoryType::Hazard);

        // [hooks].class_order override wins over the situation defaults.
        let override_order = vec![
            "observation".to_string(),
            "decision".to_string(),
            "fact".to_string(),
        ];
        let groups = group_by_class(
            &scored,
            Some(Situation::SessionStart),
            Some(&override_order),
        );
        assert_eq!(groups[0].0, Epistemic::Decision);
        assert_eq!(groups[1].0, Epistemic::Fact);
    }

    // --- Unit tests for format_class_entry (§8.2) ---

    #[test]
    fn test_format_class_entry_per_class() {
        use engramdb::types::Validity;

        // Decision with premise + watch globs.
        let mut decision = Memory::new(
            MemoryType::Decision,
            "Pin ort to rc.12",
            "content",
            Provenance::human(),
        );
        decision.valid_while = Some(Validity {
            premise: Some("rc.13 breaks the static build".into()),
            invalidated_by: vec!["Cargo.lock".into()],
            ..Default::default()
        });
        let lines = format_class_entry(
            &ScoredMemory {
                memory: decision,
                score: 0.9,
                score_breakdown: Default::default(),
            },
            false,
        );
        assert_eq!(lines.len(), 1, "decisions are a single atomic line");
        assert!(lines[0].contains("Pin ort to rc.12 — because rc.13 breaks the static build"));
        assert!(lines[0].contains("revisit if Cargo.lock changes"));

        // Decision WITHOUT premise: summary only, never invent a rationale.
        let bare = Memory::new(MemoryType::Decision, "Use tokio", "c", Provenance::human());
        let lines = format_class_entry(
            &ScoredMemory {
                memory: bare,
                score: 0.9,
                score_breakdown: Default::default(),
            },
            false,
        );
        assert!(!lines[0].contains("because"), "{}", lines[0]);

        // Observation: observed date, verified when set.
        let mut obs = Memory::new(MemoryType::Debug, "Flaky test", "c", Provenance::human());
        obs.created_at = "2026-06-01T00:00:00Z".parse().unwrap();
        obs.verified_at = Some("2026-07-01T00:00:00Z".parse().unwrap());
        let lines = format_class_entry(
            &ScoredMemory {
                memory: obs,
                score: 0.9,
                score_breakdown: Default::default(),
            },
            false,
        );
        assert!(lines[0].contains("(observed 2026-06-01, verified 2026-07-01"));

        // Fact: verified tag only when set; compact drops the preview.
        let mut fact = Memory::new(
            MemoryType::Hazard,
            "Index deletion loses data",
            "A longer body explaining the hazard in detail",
            Provenance::human(),
        );
        fact.verified_at = Some("2026-07-10T00:00:00Z".parse().unwrap());
        let scored = ScoredMemory {
            memory: fact,
            score: 0.9,
            score_breakdown: Default::default(),
        };
        let full = format_class_entry(&scored, false);
        assert_eq!(full.len(), 2, "fact carries a preview when not compacting");
        assert!(full[0].contains("(verified 2026-07-10)"));
        let compact = format_class_entry(&scored, true);
        assert_eq!(compact.len(), 1, "compact fact drops the preview");
    }

    #[test]
    fn test_budget_decision_atomicity_and_fact_compression() {
        use engramdb::types::Validity;

        // A decision whose because-clause makes it long.
        let mut decision = Memory::new(
            MemoryType::Decision,
            "Long decision summary here",
            "content",
            Provenance::human(),
        );
        decision.valid_while = Some(Validity {
            premise: Some("a very long premise explaining the constraint in detail".into()),
            ..Default::default()
        });
        let scored = vec![ScoredMemory {
            memory: decision,
            score: 0.9,
            score_breakdown: Default::default(),
        }];
        // Budget too small for the full atomic line (but larger than the
        // header): the decision is skipped WHOLE — no truncated rationale.
        let ctx = format_class_context_with_budget("[H]", &scored, 60, None, None);
        assert!(
            !ctx.contains("because"),
            "decision must never be truncated mid-clause: {ctx}"
        );
        assert!(ctx.contains("omitted"));

        // A fact with a long preview under a budget that fits only the
        // summary line: preview dropped first, summary still included.
        let fact = Memory::new(
            MemoryType::Context,
            "Short fact",
            "An extremely long body preview that will not fit the budget at all xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            Provenance::human(),
        );
        let scored = vec![ScoredMemory {
            memory: fact,
            score: 0.9,
            score_breakdown: Default::default(),
        }];
        let ctx = format_class_context_with_budget("[H]", &scored, 80, None, None);
        assert!(ctx.contains("Short fact"), "{ctx}");
        assert!(!ctx.contains("extremely long body"), "{ctx}");
        assert!(
            !ctx.contains("omitted"),
            "compressed fact still counts as included: {ctx}"
        );
    }

    #[test]
    fn test_suppress_task_scoped_memories() {
        use engramdb::types::{Generality, Validity};

        let mut task_scoped = Memory::new(
            MemoryType::Decision,
            "Task-local decision",
            "c",
            Provenance::human(),
        );
        task_scoped.valid_while = Some(Validity {
            origin_task: Some("some-task".into()),
            generality: Generality::Task,
            ..Default::default()
        });
        let project_wide = Memory::new(
            MemoryType::Decision,
            "Project-wide decision",
            "c",
            Provenance::human(),
        );
        let memories: Vec<ScoredMemory> = [task_scoped, project_wide]
            .into_iter()
            .map(|memory| ScoredMemory {
                memory,
                score: 0.9,
                score_breakdown: Default::default(),
            })
            .collect();
        let kept = suppress_task_scoped(memories, None);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].memory.summary, "Project-wide decision");
    }

    #[test]
    fn test_infer_situation_keyword_tables() {
        assert_eq!(
            infer_situation("why does the test panic here?"),
            Some(Situation::Debugging)
        );
        assert_eq!(
            infer_situation("Should we choose sqlite vs postgres?"),
            Some(Situation::DesignChoice)
        );
        // Debugging wins when both match (checked first).
        assert_eq!(
            infer_situation("error in the design"),
            Some(Situation::Debugging)
        );
        assert_eq!(infer_situation("add a new field to the struct"), None);
    }

    #[test]
    fn test_extract_prompt() {
        assert_eq!(
            extract_prompt(r#"{"prompt":"why does this fail?"}"#),
            Some("why does this fail?".to_string())
        );
        assert_eq!(extract_prompt(r#"{"prompt":"  "}"#), None);
        assert_eq!(extract_prompt("not json"), None);
        assert_eq!(extract_prompt(r#"{}"#), None);
    }

    // --- Unit tests for build_session_start_context (reflection nudge) ---

    #[test]
    fn test_session_start_context_appends_reflection_nudge() {
        let mem = Memory::new(
            MemoryType::Decision,
            "Use async everywhere",
            "All I/O operations should use async/await",
            Provenance::human(),
        );
        let scored = vec![ScoredMemory {
            memory: mem,
            score: 0.9,
            score_breakdown: Default::default(),
        }];
        let ctx = build_session_start_context(&scored);
        assert!(ctx.contains("[EngramDB] Key project memories:"));
        assert!(ctx.contains("Use async everywhere"));
        assert!(ctx.contains("When you finish the task"));
    }

    #[test]
    fn test_session_start_context_nudge_only_when_no_memories() {
        let ctx = build_session_start_context(&[]);
        assert!(ctx.contains("When you finish the task"));
        assert!(!ctx.contains("Key project memories"));
    }

    #[test]
    fn test_session_start_nudge_is_mcp_agnostic() {
        // The SessionStart hook can run in a hooks-only install with no MCP
        // server connected, so the nudge must not reference MCP tool names
        // or assume MCP is available.
        let lower = REFLECTION_NUDGE.to_lowercase();
        for forbidden in ["query", "create", "challenge", "mcp"] {
            assert!(
                !lower.contains(forbidden),
                "hook nudge must be MCP-agnostic; found '{forbidden}' in: {REFLECTION_NUDGE}"
            );
        }
        assert!(REFLECTION_NUDGE.contains("EngramDB"));
    }

    // --- Unit tests for truncate_content ---

    #[test]
    fn test_truncate_content_short() {
        assert_eq!(truncate_content("hello world", 200), "hello world");
    }

    #[test]
    fn test_truncate_content_long() {
        let long = "a".repeat(300);
        let result = truncate_content(&long, 200);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 203); // 200 + "..."
    }

    #[test]
    fn test_truncate_content_newlines_collapsed() {
        let content = "line1\nline2\nline3";
        assert_eq!(truncate_content(content, 200), "line1 line2 line3");
    }

    // --- Unit tests for build_hook_response ---

    #[test]
    fn test_build_hook_response_structure() {
        let json_str = build_hook_response("PreToolUse", "test context").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let hook_output = &parsed["hookSpecificOutput"];
        assert_eq!(hook_output["hookEventName"], "PreToolUse");
        assert_eq!(hook_output["additionalContext"], "test context");
    }

    #[test]
    fn test_build_hook_response_session_start() {
        let json_str = build_hook_response("SessionStart", "test context").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let hook_output = &parsed["hookSpecificOutput"];
        assert_eq!(hook_output["hookEventName"], "SessionStart");
        assert_eq!(hook_output["additionalContext"], "test context");
    }

    #[test]
    fn test_build_hook_response_special_characters() {
        let ctx = "line1\nline2\ttab \"quotes\"";
        let json_str = build_hook_response("PreToolUse", ctx).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap(),
            ctx
        );
    }

    // --- Integration tests for the new hook events (§8.5) ---

    #[tokio::test]
    async fn post_tool_use_warns_on_watch_match_and_respects_validity() {
        use engramdb::types::Validity;

        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("Cargo.lock"), "").unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // A live memory watching Cargo.lock…
        let mut watcher = Memory::new(
            MemoryType::Decision,
            "Pin ort while rc.12",
            "content",
            Provenance::human(),
        );
        watcher.id = "watch-live".to_string();
        watcher.valid_while = Some(Validity {
            invalidated_by: vec!["Cargo.lock".into()],
            ..Default::default()
        });
        store.create(&watcher).await.unwrap();

        // …and an INVALIDATED one watching the same path (must not warn —
        // §2.4 validity guard / §14.14).
        let mut dead = Memory::new(
            MemoryType::Decision,
            "Old pin decision",
            "content",
            Provenance::human(),
        );
        dead.id = "watch-dead".to_string();
        dead.valid_while = Some(Validity {
            invalidated_by: vec!["Cargo.lock".into()],
            ..Default::default()
        });
        dead.invalidated_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
        store.create(&dead).await.unwrap();

        let abs = temp_dir.path().join("Cargo.lock");
        let input = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": abs.to_str().unwrap() }
        })
        .to_string();

        let out = process_post_tool_use(&input, temp_dir.path())
            .await
            .unwrap()
            .expect("watch match must warn");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("may invalidate memory"), "{ctx}");
        assert!(ctx.contains("Pin ort while rc.12"), "{ctx}");
        assert!(
            !ctx.contains("Old pin decision"),
            "invalidated watcher must stay silent: {ctx}"
        );

        // An edit that matches no watcher is silent.
        std::fs::write(temp_dir.path().join("README.md"), "").unwrap();
        let abs = temp_dir.path().join("README.md");
        let input = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": abs.to_str().unwrap() }
        })
        .to_string();
        assert!(process_post_tool_use(&input, temp_dir.path())
            .await
            .unwrap()
            .is_none());

        // Malformed stdin is silent, never an error.
        assert!(process_post_tool_use("not json", temp_dir.path())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn user_prompt_submit_surfaces_keyword_matches() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut mem = Memory::new(
            MemoryType::Convention,
            "Nextest is required for the test suite",
            "Use cargo nextest run, never cargo test",
            Provenance::human(),
        );
        mem.criticality = 0.9;
        store.create(&mem).await.unwrap();

        let input = serde_json::json!({ "prompt": "how do I run the nextest suite?" }).to_string();
        let out = process_user_prompt_submit(&input, temp_dir.path())
            .await
            .unwrap()
            .expect("keyword match should surface");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("Nextest is required"), "{ctx}");

        // No keyword overlap ⇒ silent.
        let input = serde_json::json!({ "prompt": "completely unrelated zebra topic" }).to_string();
        let out = process_user_prompt_submit(&input, temp_dir.path())
            .await
            .unwrap();
        assert!(out.is_none());

        // Empty/malformed stdin ⇒ silent.
        assert!(process_user_prompt_submit("", temp_dir.path())
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_new_hook_subcommands_parse() {
        use crate::app::{Cli, Command, HookCommand};
        use clap::Parser;

        for (arg, expect) in [
            ("user-prompt-submit", "UserPromptSubmit"),
            ("post-tool-use", "PostToolUse"),
            ("session-end", "SessionEnd"),
            ("pre-compact", "PreCompact"),
        ] {
            let cli = Cli::try_parse_from(["engramdb", "hook", arg]).unwrap();
            match cli.command {
                Command::Hook { command } => {
                    let name = match command {
                        HookCommand::UserPromptSubmit => "UserPromptSubmit",
                        HookCommand::PostToolUse => "PostToolUse",
                        HookCommand::SessionEnd => "SessionEnd",
                        HookCommand::PreCompact => "PreCompact",
                        _ => "other",
                    };
                    assert_eq!(name, expect);
                }
                _ => panic!("Expected Hook command"),
            }
        }
    }

    // --- Integration tests for process_hook_input ---

    async fn setup_store_with_memories() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        // Create the file on disk so canonicalize works in relativize_path
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.rs"), "").unwrap();

        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut mem = Memory::new(
            MemoryType::Decision,
            "Use async everywhere",
            "All I/O operations should use async/await",
            Provenance::human(),
        );
        mem.physical = vec!["src/main.rs".to_string()];
        mem.criticality = 0.9;
        store.create(&mem).await.unwrap();

        let mut mem2 = Memory::new(
            MemoryType::Hazard,
            "Avoid blocking calls in async",
            "Blocking calls in async context cause deadlocks",
            Provenance::human(),
        );
        mem2.physical = vec!["src/main.rs".to_string()];
        mem2.criticality = 0.8;
        store.create(&mem2).await.unwrap();

        // Re-open store (simulates real hook usage)
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();
        (temp_dir, store)
    }

    #[tokio::test]
    async fn test_process_hook_input_with_matching_file() {
        let (temp_dir, store) = setup_store_with_memories().await;

        let abs_path = temp_dir.path().join("src/main.rs");
        let input = serde_json::json!({
            "tool_name": "Read",
            "tool_input": { "file_path": abs_path.to_str().unwrap() }
        })
        .to_string();

        let result = process_hook_input(&input, temp_dir.path(), store)
            .await
            .unwrap();

        assert!(result.is_some());
        let json_str = result.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("[EngramDB]"));
        assert!(ctx.contains("Use async everywhere"));
    }

    #[tokio::test]
    async fn test_process_hook_input_with_unrelated_file() {
        let (temp_dir, store) = setup_store_with_memories().await;

        let input = serde_json::json!({
            "tool_name": "Read",
            "tool_input": { "file_path": "/some/other/unrelated/path.txt" }
        })
        .to_string();

        let result = process_hook_input(&input, temp_dir.path(), store)
            .await
            .unwrap();

        // May return None or Some depending on scope scoring — either is valid
        // The key assertion is no error/panic
        if let Some(json_str) = &result {
            let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
            assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        }
    }

    #[tokio::test]
    async fn test_process_hook_input_no_file_path() {
        let (temp_dir, store) = setup_store_with_memories().await;

        let input = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let result = process_hook_input(input, temp_dir.path(), store)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_process_hook_input_invalid_json() {
        let (temp_dir, store) = setup_store_with_memories().await;

        let result = process_hook_input("not json", temp_dir.path(), store)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_process_hook_input_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let _store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let store = MemoryStore::open(temp_dir.path()).await.unwrap();

        let input = serde_json::json!({
            "tool_name": "Read",
            "tool_input": { "file_path": "/project/src/main.rs" }
        })
        .to_string();

        let result = process_hook_input(&input, temp_dir.path(), store)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_process_hook_input_response_is_valid_json() {
        let (temp_dir, store) = setup_store_with_memories().await;

        let abs_path = temp_dir.path().join("src/main.rs");
        let input = serde_json::json!({
            "tool_name": "Write",
            "tool_input": {
                "file_path": abs_path.to_str().unwrap(),
                "content": "fn main() {}"
            }
        })
        .to_string();

        let result = process_hook_input(&input, temp_dir.path(), store)
            .await
            .unwrap();

        if let Some(json_str) = result {
            // Must be valid JSON
            let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
            // Must have correct structure
            assert!(parsed.get("hookSpecificOutput").is_some());
            assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
            assert!(parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .is_some());
        }
    }

    // --- CLI arg parsing test ---

    #[test]
    fn test_hook_pre_tool_use_command_parses() {
        use crate::app::{Cli, Command, HookCommand};
        use clap::Parser;

        let cli = Cli::try_parse_from(["engramdb", "hook", "pre-tool-use"]).unwrap();
        match cli.command {
            Command::Hook { command } => match command {
                HookCommand::PreToolUse => {} // expected
                _ => panic!("Expected PreToolUse"),
            },
            _ => panic!("Expected Hook command"),
        }
    }

    #[test]
    fn test_hook_pre_tool_use_with_dir_flag() {
        use crate::app::{Cli, Command, HookCommand};
        use clap::Parser;

        let cli =
            Cli::try_parse_from(["engramdb", "hook", "pre-tool-use", "--dir", "/tmp"]).unwrap();
        assert_eq!(cli.dir, Some(std::path::PathBuf::from("/tmp")));
        match cli.command {
            Command::Hook { command } => match command {
                HookCommand::PreToolUse => {}
                _ => panic!("Expected PreToolUse"),
            },
            _ => panic!("Expected Hook command"),
        }
    }

    #[test]
    fn test_hook_session_start_command_parses() {
        use crate::app::{Cli, Command, HookCommand};
        use clap::Parser;

        let cli = Cli::try_parse_from(["engramdb", "hook", "session-start"]).unwrap();
        match cli.command {
            Command::Hook { command } => match command {
                HookCommand::SessionStart { min_criticality } => {
                    assert!((min_criticality - 0.6).abs() < f64::EPSILON);
                }
                _ => panic!("Expected SessionStart"),
            },
            _ => panic!("Expected Hook command"),
        }
    }

    #[test]
    fn test_hook_session_start_with_custom_threshold() {
        use crate::app::{Cli, Command, HookCommand};
        use clap::Parser;

        let cli = Cli::try_parse_from([
            "engramdb",
            "hook",
            "session-start",
            "--min-criticality",
            "0.8",
        ])
        .unwrap();
        match cli.command {
            Command::Hook { command } => match command {
                HookCommand::SessionStart { min_criticality } => {
                    assert!((min_criticality - 0.8).abs() < f64::EPSILON);
                }
                _ => panic!("Expected SessionStart"),
            },
            _ => panic!("Expected Hook command"),
        }
    }

    // --- run_hook_session_start integration tests (via process_session_start) ---

    async fn store_with_criticality(crit_values: &[(f64, &str)]) -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        for (crit, summary) in crit_values {
            let mut mem = Memory::new(
                MemoryType::Decision,
                *summary,
                "body content",
                Provenance::human(),
            );
            mem.criticality = *crit;
            store.create(&mem).await.unwrap();
        }
        temp_dir
    }

    #[tokio::test]
    async fn session_start_uninitialized_store_is_silent() {
        let temp_dir = TempDir::new().unwrap();
        // No init — `MemoryStore::open` will fail and the hook must
        // swallow it (we don't want a missing store to break Claude Code).
        let out = process_session_start(temp_dir.path(), 0.5, "")
            .await
            .unwrap();
        assert!(out.is_none(), "missing store must return None silently");
    }

    #[tokio::test]
    async fn session_start_empty_store_emits_nudge_only() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let _ = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // An empty store still emits the reflection nudge on its own, but no
        // "Key project memories" block.
        let out = process_session_start(temp_dir.path(), 0.5, "")
            .await
            .unwrap()
            .expect("empty store still surfaces the reflection nudge");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.contains(REFLECTION_NUDGE),
            "empty store must still emit the reflection nudge: {ctx}"
        );
        assert!(
            !ctx.contains("Key project memories"),
            "empty store must not emit a memories block: {ctx}"
        );
    }

    #[tokio::test]
    async fn session_start_below_threshold_emits_nudge_only() {
        // Only low-criticality memories — none are surfaced at session start
        // when min_criticality is above all of them, but the nudge still fires.
        let dir = store_with_criticality(&[(0.2, "low"), (0.3, "lower")]).await;

        let out = process_session_start(dir.path(), 0.9, "")
            .await
            .unwrap()
            .expect("below-threshold store still surfaces the reflection nudge");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(
            ctx.contains(REFLECTION_NUDGE),
            "below-threshold store must still emit the reflection nudge: {ctx}"
        );
        assert!(
            !ctx.contains("Key project memories"),
            "no memory above threshold → no memories block: {ctx}"
        );
        assert!(
            !ctx.contains("low") && !ctx.contains("lower"),
            "below-threshold memories must not appear: {ctx}"
        );
    }

    #[tokio::test]
    async fn session_start_surfaces_high_criticality_memories() {
        let dir =
            store_with_criticality(&[(0.95, "Critical decision A"), (0.10, "Low-priority B")])
                .await;

        let out = process_session_start(dir.path(), 0.7, "")
            .await
            .unwrap()
            .expect("high-criticality memory should surface");

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("[EngramDB]"));
        assert!(
            ctx.contains("Critical decision A"),
            "above-threshold memory must appear in context: {}",
            ctx
        );
    }

    #[tokio::test]
    async fn session_start_min_criticality_filters_correctly() {
        // Exercise the min_criticality parameter directly: same store, two
        // different thresholds — high threshold filters out the mid memory.
        let dir = store_with_criticality(&[
            (0.95, "High criticality keeper"),
            (0.55, "Mid criticality maybe"),
        ])
        .await;

        // Lower threshold: both included.
        let permissive = process_session_start(dir.path(), 0.5, "")
            .await
            .unwrap()
            .expect("permissive threshold should surface at least one");
        let permissive_parsed: serde_json::Value = serde_json::from_str(&permissive).unwrap();
        let permissive_ctx = permissive_parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(permissive_ctx.contains("High criticality"));
        assert!(permissive_ctx.contains("Mid criticality"));

        // Higher threshold: only the top one survives.
        let strict = process_session_start(dir.path(), 0.9, "")
            .await
            .unwrap()
            .expect("strict threshold should still surface the top memory");
        let strict_parsed: serde_json::Value = serde_json::from_str(&strict).unwrap();
        let strict_ctx = strict_parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(strict_ctx.contains("High criticality"));
        assert!(
            !strict_ctx.contains("Mid criticality"),
            "min_criticality must drop mid-level memories: {}",
            strict_ctx
        );
    }
}

#[cfg(test)]
mod task_lifecycle_hook_tests {
    use super::*;
    use engramdb::storage::{task_state, InMemoryRegistry, MemoryStore};
    use engramdb::types::{Generality, Memory, MemoryType, Provenance, Validity};
    use tempfile::TempDir;

    fn task_scoped(summary: &str, task: &str) -> ScoredMemory {
        let mut m = Memory::new(MemoryType::Decision, summary, "c", Provenance::human());
        m.valid_while = Some(Validity {
            origin_task: Some(task.to_string()),
            generality: Generality::Task,
            ..Default::default()
        });
        ScoredMemory {
            memory: m,
            score: 0.9,
            score_breakdown: Default::default(),
        }
    }

    #[test]
    fn suppression_unsuppresses_matching_task() {
        let memories = vec![
            task_scoped("mine", "feat-x"),
            task_scoped("other", "feat-y"),
        ];
        // Declared task feat-x: its memories surface; feat-y stays hidden.
        let kept = suppress_task_scoped(memories, Some("feat-x"));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].memory.summary, "mine");
    }

    #[tokio::test]
    async fn session_start_unsuppresses_declared_task_memories() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let mut m = Memory::new(
            MemoryType::Decision,
            "Task-scoped decision",
            "c",
            Provenance::human(),
        );
        m.criticality = 0.9;
        m.valid_while = Some(Validity {
            origin_task: Some("feat-x".to_string()),
            generality: Generality::Task,
            ..Default::default()
        });
        store.create(&m).await.unwrap();

        // Without a mapping, the task-scoped memory is suppressed.
        let out = process_session_start(temp_dir.path(), 0.5, r#"{"session_id":"sess-1"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(!out.contains("Task-scoped decision"), "{out}");

        // Declare the task for this session → un-suppressed.
        task_state::set_current_task(temp_dir.path(), "sess-1", "feat-x").unwrap();
        let out = process_session_start(temp_dir.path(), 0.5, r#"{"session_id":"sess-1"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(out.contains("Task-scoped decision"), "{out}");

        // A different session with no mapping of its own falls back to the
        // freshest declared mapping (the MCP-tool → hook bridge): the memory
        // surfaces there too while the declaring mapping is fresh.
        let out = process_session_start(temp_dir.path(), 0.5, r#"{"session_id":"sess-2"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(out.contains("Task-scoped decision"), "{out}");

        // Once the declaring session's mapping is cleared (SessionEnd), the
        // foreign session has nothing to fall back to → suppressed again.
        task_state::clear_session_task(temp_dir.path(), "sess-1").unwrap();
        let out = process_session_start(temp_dir.path(), 0.5, r#"{"session_id":"sess-2"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(!out.contains("Task-scoped decision"), "{out}");
    }

    #[test]
    fn extract_session_id_variants() {
        assert_eq!(
            extract_session_id(r#"{"session_id":"abc"}"#),
            Some("abc".to_string())
        );
        assert_eq!(extract_session_id(r#"{"session_id":""}"#), None);
        assert_eq!(extract_session_id(r#"{}"#), None);
        assert_eq!(extract_session_id("not json"), None);
    }
}

#[cfg(test)]
mod hint_line_tests {
    use super::*;
    use engramdb::storage::{task_state, InMemoryRegistry, MemoryStore};
    use engramdb::types::{Generality, Memory, MemoryType, Provenance, Validity};
    use tempfile::TempDir;

    #[tokio::test]
    async fn session_start_hints_when_task_scoped_memories_hidden() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        for (id, task) in [("h1", "feat-a"), ("h2", "feat-b")] {
            let mut m = Memory::new(MemoryType::Decision, id, "c", Provenance::human());
            m.id = id.to_string();
            m.criticality = 0.9;
            m.valid_while = Some(Validity {
                origin_task: Some(task.to_string()),
                generality: Generality::Task,
                ..Default::default()
            });
            store.create(&m).await.unwrap();
        }

        // No declared task: both hidden → hint names the count.
        let out = process_session_start(temp_dir.path(), 0.5, r#"{"session_id":"s"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(
            out.contains("2 task-scoped memories hidden — declare task_current"),
            "{out}"
        );

        // Declaring one task un-hides its memory; hint counts the remainder.
        task_state::set_current_task(temp_dir.path(), "s", "feat-a").unwrap();
        let out = process_session_start(temp_dir.path(), 0.5, r#"{"session_id":"s"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(out.contains("h1"), "{out}");
        assert!(out.contains("1 task-scoped memories hidden"), "{out}");

        // Nothing suppressed → no hint.
        let store2 = MemoryStore::open(temp_dir.path()).await.unwrap();
        drop(store2);
        let tmp2 = TempDir::new().unwrap();
        let _ = MemoryStore::init(tmp2.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let out = process_session_start(tmp2.path(), 0.5, "")
            .await
            .unwrap()
            .unwrap();
        assert!(!out.contains("task-scoped memories hidden"), "{out}");
    }
}
