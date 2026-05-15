//! Claude Code plugin hook handlers.
//!
//! Reads hook event JSON from stdin, retrieves relevant memories,
//! and outputs additionalContext JSON to stdout.

use crate::retrieval::engine::{DetailLevel, RetrievalMode, RetrievalQuery, ScoredMemory};
use crate::storage::MemoryStore;
use crate::types::EmbeddingBackend;
use anyhow::Result;
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

/// Make a file path relative to the project directory if possible.
///
/// Canonicalizes both paths before stripping so that `--dir .` works
/// when tool input contains an absolute file path.
fn relativize_path(file_path: &str, project_dir: &Path) -> String {
    let canonical_dir = project_dir
        .canonicalize()
        .unwrap_or(project_dir.to_path_buf());
    let canonical_file = Path::new(file_path)
        .canonicalize()
        .unwrap_or(Path::new(file_path).to_path_buf());
    canonical_file
        .strip_prefix(&canonical_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| file_path.to_string())
}

/// Format scored memories into a compact additionalContext string (for PreToolUse).
fn format_additional_context(header: &str, memories: &[ScoredMemory]) -> String {
    let mut lines: Vec<String> = vec![header.into()];
    for scored in memories {
        let m = &scored.memory;
        let type_str = format!("{:?}", m.type_).to_lowercase();
        lines.push(format!(
            "- [{}] {} (criticality: {:.1}, score: {:.2})",
            type_str, m.summary, m.criticality, scored.score
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
fn format_detailed_context(header: &str, memories: &[ScoredMemory]) -> String {
    format_detailed_context_with_budget(header, memories, SESSION_CONTEXT_BUDGET)
}

/// Build the full SessionStart additional context: project memories (if any)
/// followed by the reflection nudge. When the store has no memories the nudge
/// is still emitted on its own so the agent always receives it.
fn build_session_start_context(memories: &[ScoredMemory]) -> String {
    if memories.is_empty() {
        REFLECTION_NUDGE.to_string()
    } else {
        format!(
            "{}\n\n{}",
            format_detailed_context("[EngramDB] Key project memories:", memories),
            REFLECTION_NUDGE
        )
    }
}

/// Budget-aware implementation (extracted for testability).
fn format_detailed_context_with_budget(
    header: &str,
    memories: &[ScoredMemory],
    budget: usize,
) -> String {
    // Group memories by type, preserving score-based ordering within each group.
    let groups = group_by_type(memories);

    let mut lines: Vec<String> = vec![header.into()];
    let mut used: usize = header.len();
    let mut included = 0usize;
    let total = memories.len();

    for (type_str, group) in &groups {
        let group_header = format!("\n## {} ({}):", type_str, group.len());
        let group_header_len = group_header.len() + 1; // +1 for join newline

        if used + group_header_len > budget {
            break;
        }
        lines.push(group_header);
        used += group_header_len;

        for scored in group {
            let entry = format_memory_entry(scored);
            let entry_len: usize = entry.iter().map(|l| l.len() + 1).sum();

            if used + entry_len > budget {
                break;
            }
            lines.extend(entry);
            used += entry_len;
            included += 1;
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

/// Group scored memories by type label, preserving input order within each group.
///
/// Returns groups in a deterministic order based on the first appearance of each type.
fn group_by_type(memories: &[ScoredMemory]) -> Vec<(String, Vec<&ScoredMemory>)> {
    let mut groups: Vec<(String, Vec<&ScoredMemory>)> = Vec::new();
    let mut type_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for scored in memories {
        let type_str = format!("{:?}", scored.memory.type_);
        if let Some(&idx) = type_index.get(&type_str) {
            groups[idx].1.push(scored);
        } else {
            let idx = groups.len();
            type_index.insert(type_str.clone(), idx);
            groups.push((type_str, vec![scored]));
        }
    }
    groups
}

/// Format a single memory entry as one or two lines (summary + optional preview).
fn format_memory_entry(scored: &ScoredMemory) -> Vec<String> {
    let m = &scored.memory;
    let mut meta_parts: Vec<String> = vec![format!("criticality: {:.1}", m.criticality)];
    if !m.tags.is_empty() {
        meta_parts.push(format!("tags: {}", m.tags.join(", ")));
    }
    if !m.logical.is_empty() {
        meta_parts.push(format!("scope: {}", m.logical.join(", ")));
    }
    let mut entry = vec![format!("- {} ({})", m.summary, meta_parts.join(" | "))];
    let preview = truncate_content(&m.content, 200);
    if preview != m.summary {
        entry.push(format!("  {}", preview));
    }
    entry
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
async fn process_hook_input(
    input: &str,
    dir: &Path,
    store: MemoryStore,
    embedding_backend: Option<EmbeddingBackend>,
) -> Result<Option<String>> {
    let file_path = match extract_file_path(input) {
        Some(fp) => fp,
        None => return Ok(None),
    };

    let relative_path = relativize_path(&file_path, dir);

    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path, embedding_backend).await;

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
    };

    let result = match crate::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(None);
        }
    };

    if result.memories.is_empty() {
        return Ok(None);
    }

    let context = format_additional_context(
        "[EngramDB] Relevant memories for this file:",
        &result.memories,
    );
    let json = build_hook_response("PreToolUse", &context)?;
    Ok(Some(json))
}

/// Run the PreToolUse hook handler.
///
/// Reads JSON from stdin, extracts `tool_input.file_path`,
/// retrieves relevant memories for that path, and prints
/// JSON with `additionalContext` to stdout.
pub async fn run_hook_pre_tool_use(
    dir: &Path,
    embedding_backend: Option<EmbeddingBackend>,
) -> Result<()> {
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

    if let Some(json) = process_hook_input(&input, dir, store, embedding_backend).await? {
        println!("{}", json);
    }

    Ok(())
}

/// Run the SessionStart hook handler.
///
/// Retrieves high-criticality active memories and prints them as
/// additionalContext JSON to stdout so they are surfaced at session start.
pub async fn run_hook_session_start(
    dir: &Path,
    embedding_backend: Option<EmbeddingBackend>,
    min_criticality: f64,
) -> Result<()> {
    let store = match MemoryStore::open(dir).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Hook store open failed (non-fatal): {}", e);
            return Ok(());
        }
    };

    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path, embedding_backend).await;

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
    };

    let result = match crate::ops::query_memories(&engine, &query).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Hook retrieval failed (non-fatal): {}", e);
            return Ok(());
        }
    };

    // Always emit the reflection nudge, even when the store has no memories
    // yet — the agent should still be reminded to capture durable learnings.
    let context = build_session_start_context(&result.memories);
    let json = build_hook_response("SessionStart", &context)?;
    println!("{}", json);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance};
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
    fn test_format_detailed_context_with_tags_and_scope() {
        let mut mem = Memory::new(
            MemoryType::Convention,
            "Azure DevOps PR conventions",
            "PR templates are stored in .azuredevops/pull_request_template/",
            Provenance::human(),
        );
        mem.tags = vec!["pr".into(), "azure-devops".into()];
        mem.logical = vec!["workflow.pr".into()];
        mem.criticality = 0.8;
        let scored = ScoredMemory {
            memory: mem,
            score: 0.9,
            score_breakdown: Default::default(),
        };
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &[scored]);
        assert!(ctx.contains("Convention"));
        assert!(ctx.contains("Azure DevOps PR conventions"));
        assert!(ctx.contains("tags: pr, azure-devops"));
        assert!(ctx.contains("scope: workflow.pr"));
        assert!(ctx.contains("criticality: 0.8"));
        assert!(ctx.contains("PR templates are stored in"));
    }

    #[test]
    fn test_format_detailed_context_groups_by_type() {
        let mem1 = Memory::new(
            MemoryType::Hazard,
            "Do not delete index",
            "Index deletion causes data loss",
            Provenance::human(),
        );
        let mem2 = Memory::new(
            MemoryType::Convention,
            "Always run clippy",
            "Run clippy with -D warnings",
            Provenance::human(),
        );
        let mem3 = Memory::new(
            MemoryType::Hazard,
            "Avoid force push",
            "Force push destroys history",
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
                score: 0.8,
                score_breakdown: Default::default(),
            },
            ScoredMemory {
                memory: mem3,
                score: 0.7,
                score_breakdown: Default::default(),
            },
        ];
        let ctx = format_detailed_context("[EngramDB] Key project memories:", &scored);
        // Hazard group should appear first (first in input), Convention second
        let hazard_pos = ctx.find("Hazard").unwrap();
        let convention_pos = ctx.find("Convention").unwrap();
        assert!(hazard_pos < convention_pos);
        // Both hazard memories should be under the Hazard group
        assert!(ctx.contains("Do not delete index"));
        assert!(ctx.contains("Avoid force push"));
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
    fn test_format_detailed_context_skips_content_matching_summary() {
        let mem = Memory::new(
            MemoryType::Decision,
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
        // Content line should not appear since it matches summary
        let content_lines: Vec<&str> = ctx.lines().filter(|l| l.starts_with("  ")).collect();
        assert!(content_lines.is_empty());
    }

    #[test]
    fn test_format_detailed_context_no_tags_no_scope() {
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
        assert!(!ctx.contains("tags:"));
        assert!(!ctx.contains("scope:"));
        assert!(ctx.contains("criticality: 0.5"));
    }

    // --- Unit tests for group_by_type ---

    #[test]
    fn test_group_by_type_preserves_order() {
        let mem1 = Memory::new(MemoryType::Convention, "Conv 1", "c1", Provenance::human());
        let mem2 = Memory::new(MemoryType::Hazard, "Hazard 1", "h1", Provenance::human());
        let mem3 = Memory::new(MemoryType::Convention, "Conv 2", "c2", Provenance::human());
        let scored = vec![
            ScoredMemory {
                memory: mem1,
                score: 0.9,
                score_breakdown: Default::default(),
            },
            ScoredMemory {
                memory: mem2,
                score: 0.8,
                score_breakdown: Default::default(),
            },
            ScoredMemory {
                memory: mem3,
                score: 0.7,
                score_breakdown: Default::default(),
            },
        ];
        let groups = group_by_type(&scored);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Convention");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, "Hazard");
        assert_eq!(groups[1].1.len(), 1);
    }

    // --- Unit tests for format_memory_entry ---

    #[test]
    fn test_format_memory_entry_with_all_metadata() {
        let mut mem = Memory::new(
            MemoryType::Convention,
            "Run clippy",
            "Always run clippy before committing",
            Provenance::human(),
        );
        mem.tags = vec!["ci".into()];
        mem.logical = vec!["workflow.lint".into()];
        mem.criticality = 0.9;
        let scored = ScoredMemory {
            memory: mem,
            score: 0.8,
            score_breakdown: Default::default(),
        };
        let lines = format_memory_entry(&scored);
        assert_eq!(lines.len(), 2); // summary + content preview
        assert!(lines[0].contains("tags: ci"));
        assert!(lines[0].contains("scope: workflow.lint"));
        assert!(lines[1].starts_with("  "));
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

        let result = process_hook_input(&input, temp_dir.path(), store, None)
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

        let result = process_hook_input(&input, temp_dir.path(), store, None)
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
        let result = process_hook_input(input, temp_dir.path(), store, None)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_process_hook_input_invalid_json() {
        let (temp_dir, store) = setup_store_with_memories().await;

        let result = process_hook_input("not json", temp_dir.path(), store, None)
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

        let result = process_hook_input(&input, temp_dir.path(), store, None)
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

        let result = process_hook_input(&input, temp_dir.path(), store, None)
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
        use crate::cli::app::{Cli, Command, HookCommand};
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
        use crate::cli::app::{Cli, Command, HookCommand};
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
        use crate::cli::app::{Cli, Command, HookCommand};
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
        use crate::cli::app::{Cli, Command, HookCommand};
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
}
