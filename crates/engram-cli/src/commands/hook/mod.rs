//! Claude Code plugin hook handlers.
//!
//! Reads hook event JSON from stdin, retrieves relevant memories,
//! and outputs additionalContext JSON to stdout.
//!
//! Hooks fire on every Read/Write/Edit the agent performs, so latency is
//! critical. Every handler builds its engine via
//! [`engramdb::ops::build_engine_without_providers`]: the file hooks carry no
//! query text (`query: None` → the engine's semantic step is skipped and
//! scoring is `scope_only`), UserPromptSubmit's queries are keyword-only, and
//! hooks never create memories, so no engine-side model is ever used.
//! Skipping provider resolution avoids a ~240ms ONNX session init (plus the
//! T5 encoder+decoder init) on every hook invocation. The one exception is
//! UserPromptSubmit's situation classifier, which needs the embedding model:
//! it uses a shared daemon that is already running, else loads the embedding
//! model alone in this process (~300 ms), once per prompt. It never spawns a
//! daemon (see [`SituationModel`]).

use anyhow::Result;
use engramdb::retrieval::engine::{DetailLevel, RetrievalMode, RetrievalQuery, ScoredMemory};
use engramdb::types::{Epistemic, Generality, MemoryType, Situation};
// Shared with the retrieval engine (which also relativizes `query.path`
// itself); the hook still pre-relativizes so the injected context shows
// repo-relative paths.
use engramdb::daemon::HookCall;
use engramdb::ops::Surface;
use engramdb::storage::paths::relativize_path;
use engramdb::storage::{MemoryStore, RegistryBackend};
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

mod bash_retry;
mod daemon_exec;
mod events;
mod format;
mod protocol;

use bash_retry::*;
#[cfg(test)]
use daemon_exec::*;
use events::*;
use format::*;
#[cfg(test)]
use protocol::*;

pub use daemon_exec::{
    delegate_to_daemon, DaemonHookExecutor, Delegated, SeedCache, StoreCache, StoreSource,
};
pub use events::{SituationModel, UsageSink};
pub use protocol::{
    print_hook_output, read_stdin, warn_unknown_hook, warn_unparseable_hook_args, MAX_HOOK_STDIN,
};

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

/// The wire form of a hook subcommand, or `None` for
/// [`HookCommand::Unknown`] (and a bare `engramdb hook`): that name comes from
/// a config this binary does not understand, so it is neither executed,
/// delegated nor recorded — recording it would let any string become a stats
/// key.
///
/// [`HookCommand::Unknown`]: crate::app::HookCommand::Unknown
pub fn hook_call(command: &crate::app::HookCommand) -> Option<HookCall> {
    use crate::app::HookCommand;
    Some(match command {
        HookCommand::PreToolUse => HookCall::PreToolUse,
        // Sanitized here, before the call can cross the daemon socket: JSON
        // has no NaN or infinity, so serde writes `null` and the daemon could
        // not parse the frame.
        HookCommand::SessionStart { min_criticality } => HookCall::SessionStart {
            min_criticality: sanitize_min_criticality(*min_criticality),
        },
        HookCommand::UserPromptSubmit => HookCall::UserPromptSubmit,
        HookCommand::PostToolUse => HookCall::PostToolUse,
        HookCommand::SessionEnd => HookCall::SessionEnd,
        HookCommand::PreCompact => HookCall::PreCompact,
        HookCommand::Unknown(_) => return None,
    })
}

/// What differs between running a hook in its own process and in the daemon.
pub struct HookEnv<'a> {
    pub stores: StoreSource<'a>,
    pub situation: SituationModel,
    /// Where `injected` usage events go: the JSONL log, or the daemon's
    /// buffer.
    pub usage: UsageSink<'a>,
    /// The registry SessionStart checks the project against (see
    /// [`engramdb::ops::projects::ensure_registered`]). `None` skips the
    /// check, which unit tests use so the machine registry is never read.
    pub registry: Option<&'a dyn RegistryBackend>,
}

/// Run one hook and return what it would print. The single entry point for
/// both paths: the hook's own process and the daemon.
///
/// Error behavior is each hook's own, unchanged: `pre-tool-use` and
/// `session-start` return an error (recorded as `ok = false`; the caller
/// still exits 0), the others swallow theirs.
pub async fn execute_hook(
    call: &HookCall,
    dir: &Path,
    input: &str,
    env: &HookEnv<'_>,
) -> Result<Option<String>> {
    match call {
        HookCall::PreToolUse => {
            let store = match env.stores.open(dir).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("Hook store open failed (non-fatal): {}", e);
                    return Ok(None);
                }
            };
            process_hook_input_in(input, dir, store, &env.usage).await
        }
        HookCall::SessionStart { min_criticality } => {
            let min_criticality = sanitize_min_criticality(*min_criticality);
            process_session_start_in(
                dir,
                min_criticality,
                input,
                &env.stores,
                &env.usage,
                env.registry,
            )
            .await
        }
        HookCall::UserPromptSubmit => {
            Ok(
                process_user_prompt_submit_in(input, dir, &env.situation, &env.stores, &env.usage)
                    .await
                    .unwrap_or(None),
            )
        }
        HookCall::PostToolUse => Ok(process_post_tool_use_in(input, dir, &env.stores)
            .await
            .unwrap_or(None)),
        HookCall::SessionEnd => {
            run_session_end_isolated(input, dir).await?;
            Ok(None)
        }
        HookCall::PreCompact => pre_compact(input, dir),
    }
}

/// Run a hook in this process and print its output.
pub async fn run_in_process(
    call: &HookCall,
    dir: &Path,
    input: &str,
    in_process: bool,
) -> Result<()> {
    // Best-effort: without a registry SessionStart only skips its check.
    let registry = engramdb::storage::FileRegistry::global()
        .map_err(|e| tracing::debug!("engramdb hook: registry unavailable: {e}"))
        .ok();
    let env = HookEnv {
        stores: StoreSource::Fresh,
        situation: if in_process {
            SituationModel::InProcess
        } else {
            SituationModel::DaemonOrInProcess
        },
        usage: UsageSink::Log,
        registry: registry.as_ref().map(|r| r as &dyn RegistryBackend),
    };
    let output = execute_hook(call, dir, input, &env).await?;
    print_hook_output(output.as_deref());
    Ok(())
}

/// The session id of a hook event, for hook stats.
pub fn event_session_id(input: &str) -> Option<String> {
    extract_session_id(input)
}

/// Record one hook call (count, wall time, outcome) in the project's hook
/// log. Best-effort: a failure is logged at debug level and never changes the
/// hook's exit status or output, which Claude Code parses.
///
/// `elapsed` runs from `cli::run` entry to handler return, so it includes
/// worktree resolution and config loading but not process start-up (binary
/// load, Tokio runtime start), which no in-process clock can see.
pub fn record_hook_stats(
    dir: &Path,
    name: &str,
    elapsed: std::time::Duration,
    ok: bool,
    session_id: Option<&str>,
) {
    let ms = elapsed.as_secs_f64() * 1000.0;
    if let Err(e) = engramdb::telemetry::hooks::record_hook_call(dir, name, ms, ok, session_id) {
        tracing::debug!("hook stats: could not record `{name}` (non-fatal): {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::storage::{InMemoryRegistry, MemoryStore};

    /// Every hook is recorded under its clap subcommand name, and no name is
    /// missed: a new `HookCommand` variant must get a `hook_call` arm (the
    /// match is exhaustive) whose name is the one Claude Code runs.
    #[test]
    fn hook_call_name_matches_every_subcommand_name() {
        use crate::app::HookCommand;
        let named: Vec<&str> = [
            HookCommand::PreToolUse,
            HookCommand::SessionStart {
                min_criticality: 0.6,
            },
            HookCommand::UserPromptSubmit,
            HookCommand::PostToolUse,
            HookCommand::SessionEnd,
            HookCommand::PreCompact,
        ]
        .iter()
        .map(|c| hook_call(c).expect("known hook has a call").name())
        .collect();
        let mut supported = crate::supported_hook_subcommands();
        supported.sort();
        let mut named_sorted: Vec<String> = named.iter().map(|s| s.to_string()).collect();
        named_sorted.sort();
        assert_eq!(named_sorted, supported);
        assert_eq!(
            hook_call(&HookCommand::Unknown(vec!["x".into()])),
            None,
            "unknown hook names must never become stats keys"
        );
    }

    #[test]
    fn record_hook_stats_writes_one_line_in_an_initialized_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".engramdb/memories")).unwrap();
        record_hook_stats(
            tmp.path(),
            "pre-tool-use",
            std::time::Duration::from_millis(7),
            true,
            Some("sess-7"),
        );
        let loaded = engramdb::telemetry::hooks::load_hook_calls(tmp.path()).unwrap();
        assert_eq!(loaded.records.len(), 1);
        assert_eq!(loaded.records[0].hook, "pre-tool-use");
        assert!((loaded.records[0].ms - 7.0).abs() < 1e-9);
        assert_eq!(loaded.records[0].session_id.as_deref(), Some("sess-7"));
    }

    /// A NaN or infinite threshold is sanitized before the call is built, so
    /// it survives the JSON round-trip to the daemon (JSON has no NaN; serde
    /// writes `null`, which the daemon cannot parse).
    #[test]
    fn hook_call_sanitizes_min_criticality_for_the_wire() {
        use crate::app::HookCommand;
        for (raw, want) in [(f64::NAN, 0.6), (f64::INFINITY, 1.0), (-f64::INFINITY, 0.0)] {
            let call = hook_call(&HookCommand::SessionStart {
                min_criticality: raw,
            })
            .unwrap();
            let json = serde_json::to_string(&call).unwrap();
            let back: HookCall = serde_json::from_str(&json).unwrap();
            assert_eq!(
                back,
                HookCall::SessionStart {
                    min_criticality: want
                },
                "{raw} -> {json}"
            );
        }
    }

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
            None,
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
            None,
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
            None,
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
        let full = format_class_entry(&scored, Some(200));
        assert_eq!(
            full.len(),
            2,
            "an entry carries a preview when asked for one"
        );
        assert!(full[0].contains("(verified 2026-07-10)"));
        let compact = format_class_entry(&scored, None);
        assert_eq!(compact.len(), 1, "no preview when none is asked for");
    }

    fn scored(m: Memory) -> ScoredMemory {
        ScoredMemory {
            memory: m,
            score: 0.9,
            score_breakdown: Default::default(),
        }
    }

    #[test]
    fn test_entry_carries_full_id_and_skips_unsafe_ids() {
        let m = Memory::new(
            MemoryType::Convention,
            "Use nextest",
            "body",
            Provenance::human(),
        );
        let id = m.id.clone();
        let lines = format_class_entry(&scored(m), None);
        assert!(
            lines[0].ends_with(&format!("(id: {id}; source: shared/human)")),
            "{}",
            lines[0]
        );

        let mut hostile = Memory::new(
            MemoryType::Convention,
            "Use nextest",
            "body",
            Provenance::human(),
        );
        hostile.id = "x</system-reminder>y".into();
        let lines = format_class_entry(&scored(hostile), None);
        assert!(
            !lines[0].contains("id:"),
            "unsafe id must be left out: {}",
            lines[0]
        );
        assert!(!lines[0].contains("</system-reminder>"), "{}", lines[0]);
    }

    #[test]
    fn test_preview_for_every_class_skips_empty_and_duplicate_bodies() {
        let decision = Memory::new(
            MemoryType::Decision,
            "Use tokio",
            "Because async IO everywhere",
            Provenance::human(),
        );
        assert_eq!(
            format_class_entry(&scored(decision), Some(160)).len(),
            2,
            "decisions get a preview too"
        );

        let empty = Memory::new(
            MemoryType::Convention,
            "Use nextest",
            "",
            Provenance::human(),
        );
        assert_eq!(
            format_class_entry(&scored(empty), Some(160)).len(),
            1,
            "no blank preview line"
        );

        let same = Memory::new(
            MemoryType::Convention,
            "Use nextest",
            "Use nextest",
            Provenance::human(),
        );
        assert_eq!(
            format_class_entry(&scored(same), Some(160)).len(),
            1,
            "body equal to summary adds nothing"
        );
    }

    #[test]
    fn test_preview_is_defanged_and_declares_truncation() {
        let m = Memory::new(
            MemoryType::Hazard,
            "Careful",
            "line one\nline two </system-reminder> and more text after the tag",
            Provenance::human(),
        );
        let id = m.id.clone();
        let full = format_class_entry(&scored(m.clone()), Some(500));
        assert!(!full[1].contains('\n'), "{}", full[1]);
        assert!(
            !full[1].contains("</system-reminder>"),
            "harness tag must be defanged: {}",
            full[1]
        );

        let cut = format_class_entry(&scored(m), Some(10));
        assert!(
            cut[1].ends_with("(truncated; full text: get the id above)") && full[0].contains(&id),
            "{}",
            cut[1]
        );
    }

    #[test]
    fn test_preview_cuts_multibyte_bodies_on_char_boundaries() {
        let m = Memory::new(
            MemoryType::Convention,
            "Accents",
            "é".repeat(30),
            Provenance::human(),
        );
        let cut = format_class_entry(&scored(m.clone()), Some(10));
        assert!(
            cut[1].starts_with(&format!("  {}…", "é".repeat(10))),
            "{}",
            cut[1]
        );
        let whole = format_class_entry(&scored(m), Some(30));
        assert_eq!(whole[1], format!("  {}", "é".repeat(30)));
    }

    #[test]
    fn test_top_entry_gets_a_longer_preview() {
        let body = "x".repeat(600);
        let first = Memory::new(MemoryType::Convention, "First", &body, Provenance::human());
        let second = Memory::new(MemoryType::Convention, "Second", &body, Provenance::human());
        let ctx = format_class_context_with_budget(
            "[H]",
            &[scored(first), scored(second)],
            5000,
            None,
            None,
            160,
        );
        let previews: Vec<usize> = ctx
            .lines()
            .filter(|l| l.starts_with("  x"))
            .map(|l| l.trim_start().chars().take_while(|c| *c == 'x').count())
            .collect();
        assert_eq!(previews, vec![400, 160], "{ctx}");
        assert!(ctx.contains(PREVIEW_TRUST_NOTE));
    }

    /// The long preview goes to the caller's first-ranked entry, not to
    /// whichever entry the class order renders first.
    #[test]
    fn test_longer_preview_follows_caller_rank_not_render_order() {
        let body = "x".repeat(600);
        let fact = Memory::new(MemoryType::Convention, "Fact", &body, Provenance::human());
        let decision = Memory::new(MemoryType::Decision, "Decision", &body, Provenance::human());
        let ctx = format_class_context_with_budget(
            "[H]",
            &[scored(fact), scored(decision)],
            5000,
            Some(Situation::FileEdit),
            None,
            160,
        );
        let previews: Vec<usize> = ctx
            .lines()
            .filter(|l| l.starts_with("  x"))
            .map(|l| l.trim_start().chars().take_while(|c| *c == 'x').count())
            .collect();
        // FileEdit renders the decision first; the fact was ranked first.
        assert_eq!(previews, vec![160, 400], "{ctx}");
    }

    #[test]
    fn test_tight_budget_keeps_every_summary_and_shortens_previews() {
        let body = "y".repeat(600);
        let mems: Vec<ScoredMemory> = ["Alpha", "Beta", "Gamma"]
            .into_iter()
            .map(|t| {
                scored(Memory::new(
                    MemoryType::Convention,
                    t,
                    &body,
                    Provenance::human(),
                ))
            })
            .collect();
        let ctx = format_class_context_with_budget("[H]", &mems, 1000, None, None, 160);
        for t in ["Alpha", "Beta", "Gamma"] {
            assert!(ctx.contains(t), "{t} missing:\n{ctx}");
        }
        assert!(!ctx.contains("omitted"), "{ctx}");
        let previews: Vec<usize> = ctx
            .lines()
            .filter(|l| l.starts_with("  y"))
            .map(|l| l.trim_start().chars().take_while(|c| *c == 'y').count())
            .collect();
        assert_eq!(previews.len(), 3, "every entry keeps a body:\n{ctx}");
        assert!(
            previews[2] >= MIN_PREVIEW_CHARS && previews[2] < 160,
            "{previews:?}"
        );
        assert!(ctx.len() <= 1000, "{}", ctx.len());
    }

    #[test]
    fn test_fact_with_premise_shows_it_as_a_check() {
        use engramdb::types::Validity;
        let mut m = Memory::new(
            MemoryType::Convention,
            "Use the old decorator",
            "body",
            Provenance::human(),
        );
        m.valid_while = Some(Validity {
            premise: Some("prod pins v1".to_string()),
            ..Default::default()
        });
        let line = &format_class_entry(&scored(m.clone()), None)[0];
        assert!(
            line.contains("holds only while prod pins v1: check that before following it"),
            "{line}"
        );
        m.valid_while = None;
        assert!(!format_class_entry(&scored(m), None)[0].contains("holds only while"));
    }

    #[test]
    fn test_preview_chars_zero_shows_summaries_only() {
        let m = Memory::new(
            MemoryType::Convention,
            "First",
            "a body",
            Provenance::human(),
        );
        let ctx = format_class_context_with_budget("[H]", &[scored(m)], 5000, None, None, 0);
        assert!(!ctx.contains("a body"), "{ctx}");
        assert!(!ctx.contains(PREVIEW_TRUST_NOTE), "{ctx}");
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
        let ctx =
            format_class_context_with_budget("[H]", &scored, 60, None, None, DEFAULT_PREVIEW_CHARS);
        assert!(
            !ctx.contains("because"),
            "decision must never be truncated mid-clause: {ctx}"
        );
        assert!(ctx.contains("omitted"));

        // A fact with a long preview under a budget that fits only the
        // summary line (which now carries the 36-char id): preview dropped
        // first, summary still included.
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
        let ctx = format_class_context_with_budget(
            "[H]",
            &scored,
            120,
            None,
            None,
            DEFAULT_PREVIEW_CHARS,
        );
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

    #[test]
    fn command_keys_identify_the_script_not_its_flags_or_environment() {
        let k = |c: &str| command_keys(c);
        assert_eq!(
            k("python scripts/backfill_tax.py --month 2026-09"),
            ["scripts/backfill_tax.py"]
        );
        assert_eq!(
            k("cd /tmp/ws && TZ=UTC python3 ./scripts/backfill_tax.py --month 2026-09 --batch-size 500"),
            ["scripts/backfill_tax.py"]
        );
        assert_eq!(
            k("LEDGERLINE_ENV=dev python scripts/seed_dev.py; ls -la var"),
            ["scripts/seed_dev.py"]
        );
        assert_eq!(k("python -m pytest -q tests/test_currency.py"), ["pytest"]);
        assert_eq!(k("pytest tests/"), ["pytest"]);
        assert_eq!(k("make test-fast 2>&1 | tail -20"), ["make test-fast"]);
        assert_eq!(k("timeout 60 cargo build --release"), ["cargo build"]);
        assert_eq!(
            k("python3.11 -m unittest tests.test_statements"),
            ["unittest"]
        );
        // Probes and inline code are not task commands.
        assert!(k("ls var; cat docs/testing.md | head").is_empty());
        assert!(k("python -c 'import sqlite3'").is_empty());
        assert!(k("sqlite3 var/dev.sqlite3 .tables").is_empty());
    }

    #[test]
    fn command_keys_ignore_quoted_text_heredocs_and_inline_code() {
        let k = |c: &str| command_keys(c);
        // Claude Code's commit command: the message is data, not commands.
        let commit = "git add -A && git commit -m \"$(cat <<'EOF'\nFix the thing; really\n\nCo-Authored-By: X <x@y>\nEOF\n)\" && cargo test";
        assert_eq!(k(commit), ["cargo test"]);
        let script = "python3 - <<'PY'\nimport sys\nfrom x import y\nPY\nmake check";
        assert_eq!(k(script), ["make check"]);
        assert!(k("node -e \"require('fs'); console.log(1)\"").is_empty());
        assert!(k("ruby -e 'puts 1'").is_empty());
        assert_eq!(k("python scripts/x.py \"a;b|c\" --flag"), ["scripts/x.py"]);
        assert_eq!(k("cat <<< \"$X\" | jq ."), Vec::<String>::new());
    }

    /// Generated command lines: whatever the text, `command_keys` returns
    /// the same keys every time, each a plausible program name no longer
    /// than the command, distinct, and at most one per segment; and an
    /// interpreter's script is its key whatever environment, wrapper, flags
    /// or trailing probe surround it.
    #[test]
    fn the_command_key_is_stable_and_bounded_for_generated_commands() {
        const TOKENS: &[&str] = &[
            "python",
            "python3.11",
            "node",
            "bash",
            "cargo",
            "make",
            "npm",
            "run",
            "test",
            "build",
            "-m",
            "-c",
            "-e",
            "--flag",
            "x=1",
            "TZ=UTC",
            "env",
            "sudo",
            "timeout",
            "60",
            "./scripts/a.py",
            "/usr/bin/env",
            "2>&1",
            ">",
            "<",
            "<<'EOF'",
            "EOF",
            "<<<",
            ";",
            "|",
            "&&",
            "||",
            "&",
            "\n",
            "\"",
            "'",
            "(",
            ")",
            "$(",
            "`",
            "ls",
            "tail",
            "-5",
            "é",
            "中文",
            "\u{202e}",
            "\u{0}",
            "\\",
            "#",
            "*",
            "~",
            "a",
            "b.c",
        ];
        let mut g = Gen(0xc0ff_ee00_1234_5678);
        for case in 0..2000 {
            let n = g.below(16);
            let command: String = (0..n)
                .map(|_| {
                    let sep = [" ", "", "  ", "\t"][g.below(4)];
                    format!("{}{sep}", TOKENS[g.below(TOKENS.len())])
                })
                .collect();
            let keys = command_keys(&command);
            assert_eq!(keys, command_keys(&command), "case {case}: {command:?}");
            let segments = 1 + command.matches(['\n', ';', '|', '&']).count();
            assert!(
                keys.len() <= segments,
                "case {case}: {command:?} -> {keys:?}"
            );
            for (i, key) in keys.iter().enumerate() {
                assert!(plausible_key(key), "case {case}: {key:?} from {command:?}");
                assert!(
                    key.len() <= command.len(),
                    "case {case}: {key:?} from {command:?}"
                );
                assert!(!keys[..i].contains(key), "case {case}: duplicate {key:?}");
            }
        }

        const INTERPS: &[&str] = &["python", "python3", "python3.12", "node", "bash", "ruby"];
        const PREFIXES: &[&str] = &["", "TZ=UTC ", "A=1 B=2 ", "env ", "sudo ", "timeout 60 "];
        const SUFFIXES: &[&str] = &[
            "",
            " --month 2026-09",
            " -v -x",
            " 2>&1 | tail -5",
            " > out.log",
        ];
        for case in 0..500 {
            let word: String = (0..1 + g.below(8))
                .map(|_| (b'a' + g.below(26) as u8) as char)
                .collect();
            let script = format!("scripts/{word}.py");
            let command = format!(
                "{}{} {script}{}",
                PREFIXES[g.below(PREFIXES.len())],
                INTERPS[g.below(INTERPS.len())],
                SUFFIXES[g.below(SUFFIXES.len())]
            );
            assert_eq!(command_keys(&command), [script], "case {case}: {command:?}");
        }
    }

    #[test]
    fn command_keys_cover_common_runners() {
        let k = |c: &str| command_keys(c);
        assert_eq!(k("npm run lint"), ["npm run lint"]);
        assert_eq!(k("npm run build -- --prod"), ["npm run build"]);
        assert_eq!(k("npm test"), ["npm test"]);
        assert_eq!(k("go test ./..."), ["go test"]);
        assert_eq!(k("uv run pytest -x"), ["uv run pytest"]);
        assert_eq!(k("bundle exec rspec spec/a_spec.rb"), ["bundle exec rspec"]);
        assert_eq!(k("docker compose up -d db"), ["docker compose up"]);
        assert_eq!(
            k("set -o pipefail; cargo test 2>&1 | tail -5"),
            ["cargo test"]
        );
    }

    fn bash_event(session: &str, event: &str, command: &str, stdout: &str) -> String {
        serde_json::json!({
            "session_id": session,
            "hook_event_name": event,
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "tool_response": { "stdout": stdout, "stderr": "" },
        })
        .to_string()
    }

    #[tokio::test]
    async fn bash_retry_prompts_once_after_a_failed_command_succeeds() {
        let project = TempDir::new().unwrap();
        MemoryStore::init(project.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let p = project.path();
        let run = |event: &str, cmd: &str, out: &str| {
            let input = bash_event("s-1", event, cmd, out);
            async move { process_post_tool_use(&input, p).await.unwrap() }
        };

        // Probes and unrelated successes say nothing.
        assert!(run("PostToolUseFailure", "ls var", "").await.is_none());
        assert!(run("PostToolUse", "python scripts/seed_dev.py", "ok")
            .await
            .is_none());

        assert!(run("PostToolUseFailure", "python scripts/seed_dev.py", "")
            .await
            .is_none());
        assert!(
            run("PostToolUse", "python scripts/migrate_dev.py", "applied")
                .await
                .is_none()
        );
        let out = run(
            "PostToolUse",
            "LEDGERLINE_ENV=dev python scripts/seed_dev.py",
            "loaded",
        )
        .await
        .expect("the fixed command prompts");
        assert!(
            out.contains("`scripts/seed_dev.py`") && out.contains("Save it yourself"),
            "{out}"
        );
        assert!(out.contains("\"hookEventName\":\"PostToolUse\""), "{out}");

        // Once per key per session.
        assert!(run("PostToolUseFailure", "python scripts/seed_dev.py", "")
            .await
            .is_none());
        assert!(run("PostToolUse", "python scripts/seed_dev.py", "loaded")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn bash_retry_stays_quiet_in_a_red_green_loop() {
        let project = TempDir::new().unwrap();
        MemoryStore::init(project.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let p = project.path();
        let fail = bash_event("s-4", "PostToolUseFailure", "cargo test", "");
        assert!(process_post_tool_use(&fail, p).await.unwrap().is_none());
        let edit = serde_json::json!({
            "session_id": "s-4",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": p.join("src/lib.rs").to_str().unwrap() },
        })
        .to_string();
        let _ = process_post_tool_use(&edit, p).await.unwrap();
        let pass = bash_event("s-4", "PostToolUse", "cargo test", "ok");
        assert!(
            process_post_tool_use(&pass, p).await.unwrap().is_none(),
            "fail, edit, pass is a code fix, not a hidden cause"
        );
        // The key is not blocked: a later failure fixed without an edit prompts.
        assert!(process_post_tool_use(&fail, p).await.unwrap().is_none());
        assert!(process_post_tool_use(&pass, p).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn bash_retry_keeps_subagent_records_apart() {
        let project = TempDir::new().unwrap();
        MemoryStore::init(project.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let p = project.path();
        let with_agent = |event: &str, out: &str| {
            let mut v: serde_json::Value =
                serde_json::from_str(&bash_event("s-5", event, "make deploy", out)).unwrap();
            v["agent_id"] = "a1b2".into();
            v.to_string()
        };
        let parent_fail = bash_event("s-5", "PostToolUseFailure", "make deploy", "");
        assert!(process_post_tool_use(&parent_fail, p)
            .await
            .unwrap()
            .is_none());
        // The subagent's success is not the parent's fix.
        assert!(process_post_tool_use(&with_agent("PostToolUse", "ok"), p)
            .await
            .unwrap()
            .is_none());
        assert!(
            process_post_tool_use(&with_agent("PostToolUseFailure", ""), p)
                .await
                .unwrap()
                .is_none()
        );
        assert!(process_post_tool_use(&with_agent("PostToolUse", "ok"), p)
            .await
            .unwrap()
            .is_some());
    }

    #[test]
    fn context_id_separates_subagents() {
        assert_eq!(
            extract_context_id(r#"{"session_id":"s"}"#).as_deref(),
            Some("s")
        );
        assert_eq!(
            extract_context_id(r#"{"session_id":"s","agent_id":"a9"}"#).as_deref(),
            Some("s_a9")
        );
        assert_eq!(extract_context_id(r#"{"agent_id":"a9"}"#), None);
    }

    #[tokio::test]
    async fn bash_retry_sees_a_failure_masked_by_a_pipe() {
        let project = TempDir::new().unwrap();
        MemoryStore::init(project.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let p = project.path();
        let masked = bash_event(
            "s-2",
            "PostToolUse",
            "make test-fast 2>&1 | tail -5",
            "E   FileNotFoundError: [Errno 2] No such file or directory\nmake: *** [Makefile:4: test-fast] Error 1",
        );
        assert!(process_post_tool_use(&masked, p).await.unwrap().is_none());
        let fixed = bash_event(
            "s-2",
            "PostToolUse",
            "make test-fast 2>&1 | tail -5",
            "4 passed",
        );
        assert!(process_post_tool_use(&fixed, p).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn bash_retry_stays_out_of_uninitialized_directories() {
        let dir = TempDir::new().unwrap();
        let input = bash_event("s-3", "PostToolUseFailure", "python x.py", "");
        assert!(process_post_tool_use(&input, dir.path())
            .await
            .unwrap()
            .is_none());
        assert!(!dir.path().join(".engramdb").exists());
    }

    #[tokio::test]
    async fn post_tool_use_notes_auto_memory_writes() {
        let project = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(project.path(), &registry).await.unwrap();
        // Claude Code's layout: <config>/projects/<enc>/<session>.jsonl and memory/ beside it.
        let config = TempDir::new().unwrap();
        let session_dir = config.path().join("projects").join("-enc-project");
        let memory_dir = session_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        let transcript = session_dir.join("abc.jsonl");
        std::fs::write(&transcript, "").unwrap();
        let note = memory_dir.join("jobs.md");
        std::fs::write(&note, "use jobs.enqueue").unwrap();
        let index = memory_dir.join("MEMORY.md");
        std::fs::write(&index, "- jobs.md").unwrap();
        let src = project.path().join("app.py");
        std::fs::write(&src, "x = 1").unwrap();
        let event = |path: &Path| {
            serde_json::json!({
                "transcript_path": transcript.to_str().unwrap(),
                "tool_name": "Write",
                "tool_input": { "file_path": path.to_str().unwrap() }
            })
            .to_string()
        };

        let out = process_post_tool_use(&event(&note), project.path())
            .await
            .unwrap()
            .expect("an auto-memory note gets the routing note");
        assert!(out.contains(AUTO_MEMORY_NOTE_PREFIX), "{out}");
        assert!(out.contains("(jobs.md)"), "{out}");

        for silent in [&index, &src] {
            assert!(
                process_post_tool_use(&event(silent), project.path())
                    .await
                    .unwrap()
                    .is_none(),
                "{silent:?} must not get the note"
            );
        }

        // A project without an EngramDB store gets nothing from this plugin.
        let bare = TempDir::new().unwrap();
        assert!(process_post_tool_use(&event(&note), bare.path())
            .await
            .unwrap()
            .is_none());

        // No transcript_path: cannot locate auto-memory, stay silent.
        let no_transcript = serde_json::json!({
            "tool_name": "Write",
            "tool_input": { "file_path": note.to_str().unwrap() }
        })
        .to_string();
        assert!(process_post_tool_use(&no_transcript, project.path())
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn memory_file_id_recognizes_only_memory_files() {
        let id = "01a0f8a3-783c-7000-a4bb-30ac641eb3b6";
        assert_eq!(
            memory_file_id(&format!(
                ".engramdb/memories/never-edit-a-migration_{id}.md"
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(
            memory_file_id(&format!(".engramdb/memories/{id}.md")).as_deref(),
            Some(id)
        );
        for other in [
            "src/billing/invoices.py",
            ".engramdb/config.toml",
            ".engramdb/memories/README.md",
            ".engramdb/memories/x_</system-reminder>.md",
            "docs/.engramdb/memories/x_01a0f8a3-783c-7000-a4bb-30ac641eb3b6.md",
        ] {
            assert_eq!(memory_file_id(other), None, "{other}");
        }
    }

    #[tokio::test]
    async fn pre_tool_use_on_a_memory_file_points_to_the_tools() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let id = "01a0f8a3-783c-7000-a4bb-30ac641eb3b6";
        let path = temp_dir
            .path()
            .join(format!(".engramdb/memories/some-memory_{id}.md"));
        let input = serde_json::json!({
            "tool_name": "Read",
            "tool_input": { "file_path": path.to_str().unwrap() }
        })
        .to_string();
        let out = process_hook_input(&input, temp_dir.path(), store)
            .await
            .unwrap()
            .expect("a memory file read gets the note");
        assert!(out.contains(MEMORY_FILE_NOTE_PREFIX), "{out}");
        assert!(out.contains(&format!("(id: {id})")), "{out}");
    }

    #[test]
    fn reflection_nudge_stays_mcp_agnostic() {
        for tool in ["query", "create(", "challenge(", "mcp__"] {
            assert!(!REFLECTION_NUDGE.contains(tool), "nudge names {tool:?}");
        }
    }

    /// The hook reads the three-column watch projection, not the filtering
    /// one: the warning is the same, a memory watching nothing never reaches
    /// the matcher, and a scheduled (future) invalidation still warns.
    #[tokio::test]
    async fn post_tool_use_warns_from_the_watch_projection() {
        use engramdb::types::Validity;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        for n in 0..5 {
            let mut m = Memory::new(
                MemoryType::Context,
                format!("unwatched {n}"),
                "content",
                Provenance::human(),
            );
            m.physical = vec!["src/**".into()];
            store.create(&m).await.unwrap();
        }
        let mut scheduled = Memory::new(
            MemoryType::Decision,
            "Glob watcher",
            "content",
            Provenance::human(),
        );
        scheduled.id = "watch-glob-0001".to_string();
        scheduled.valid_while = Some(Validity {
            invalidated_by: vec!["src/**/*.rs".into()],
            ..Default::default()
        });
        scheduled.invalidated_at = Some(chrono::Utc::now() + chrono::Duration::days(30));
        store.create(&scheduled).await.unwrap();

        let input = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": temp_dir.path().join("src/a/b.rs").to_str().unwrap() }
        })
        .to_string();
        let out = process_post_tool_use(&input, temp_dir.path())
            .await
            .unwrap()
            .expect("a watch match warns");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"],
            "[EngramDB]\n⚠ this edit may invalidate memory watch-glob-00 ('Glob watcher') — verify \
             it or update/invalidate it"
        );
    }

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

    /// Embeds by keyword families, so a test decides which classifier seeds
    /// a prompt lands nearest to without loading a model.
    struct FamilyEmbedder {
        fail: bool,
    }

    const SITUATION_FAMILIES: &[&[&str]] = &[
        &[
            "fail",
            "broke",
            "segfault",
            "hang",
            "exception",
            "bug",
            "wrong",
            "slow",
            "trace",
        ],
        &[
            "better",
            "tradeoff",
            "compare",
            "versus",
            "decide",
            "recommend",
            "pattern",
            "plan",
        ],
        &[
            "add",
            "rename",
            "write",
            "explain",
            "update",
            "run",
            "implement",
            "move",
            "show",
        ],
        &[
            "ok",
            "thank",
            "yes",
            "go ahead",
            "keep going",
            "sure",
            "nice",
            "do it",
            "cool",
        ],
    ];

    #[async_trait::async_trait]
    impl engramdb::embeddings::EmbeddingProvider for FamilyEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::ensure!(!self.fail, "stub provider failure");
            let lower = text.to_lowercase();
            let mut v: Vec<f32> = SITUATION_FAMILIES
                .iter()
                .map(|words| words.iter().filter(|w| lower.contains(*w)).count() as f32)
                .collect();
            v.push(0.01);
            Ok(v)
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(texts.len());
            for t in texts {
                out.push(self.embed(t).await?);
            }
            Ok(out)
        }
        fn dimensions(&self) -> usize {
            SITUATION_FAMILIES.len() + 1
        }
        fn max_tokens(&self) -> usize {
            512
        }
        fn model_id(&self) -> String {
            "family-stub".into()
        }
    }

    fn stub_model(fail: bool) -> SituationModel {
        SituationModel::Provider(std::sync::Arc::new(FamilyEmbedder { fail }))
    }

    #[tokio::test]
    async fn prompt_situation_uses_the_classifier_and_falls_back_to_keywords() {
        let mut config = engramdb::types::EngramConfig::default();
        // No keyword-table word: the tables see nothing.
        let prompt = "the tests hang forever on my machine";
        assert_eq!(engramdb::prompt_intent::keyword_situation(prompt), None);

        let model = stub_model(false);
        assert_eq!(
            prompt_situation(prompt, &config, &model).await,
            Some(Situation::Debugging)
        );
        // Turn control has no situation; a gate-skipped prompt falls to the
        // keyword tables, which still see a failure word.
        assert_eq!(prompt_situation("ok thanks", &config, &model).await, None);
        assert_eq!(
            prompt_situation("ok thanks sure, it panics", &config, &model).await,
            Some(Situation::Debugging)
        );

        // A failing model, a keyword-only model and the config switch all
        // fall back to the keyword tables.
        let keyword_prompt = "why does this panic";
        for model in [stub_model(true), SituationModel::KeywordOnly] {
            assert_eq!(prompt_situation(prompt, &config, &model).await, None);
            assert_eq!(
                prompt_situation(keyword_prompt, &config, &model).await,
                Some(Situation::Debugging)
            );
        }
        config.hooks.situation_classifier = false;
        assert_eq!(prompt_situation(prompt, &config, &model).await, None);
    }

    /// Counts every text it embeds.
    struct CountingTexts(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl engramdb::embeddings::EmbeddingProvider for CountingTexts {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            FamilyEmbedder { fail: false }.embed(text).await
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.0
                .fetch_add(texts.len(), std::sync::atomic::Ordering::SeqCst);
            FamilyEmbedder { fail: false }.embed_batch(texts).await
        }
        fn dimensions(&self) -> usize {
            SITUATION_FAMILIES.len() + 1
        }
        fn max_tokens(&self) -> usize {
            512
        }
        fn model_id(&self) -> String {
            "family-stub".into()
        }
    }

    /// The daemon embeds the 24 seeds once and then only each prompt, and
    /// decides exactly as the uncached classifier does.
    #[tokio::test]
    async fn daemon_executor_embeds_seeds_once() {
        let config = engramdb::types::EngramConfig::default();
        let seeds = SeedCache::default();
        let counting = std::sync::Arc::new(CountingTexts(std::sync::atomic::AtomicUsize::new(0)));
        let texts = || counting.0.load(std::sync::atomic::Ordering::SeqCst);
        let uncached = stub_model(false);
        let seed_count = engramdb::prompt_intent::PrototypeParams::default().seeds_per_class * 4;

        let model = seeds.model_for(counting.clone()).await;
        assert!(matches!(model, SituationModel::Cached { .. }));
        let prompt = "the tests hang forever on my machine";
        assert_eq!(
            prompt_situation(prompt, &config, &model).await,
            prompt_situation(prompt, &config, &uncached).await
        );
        assert_eq!(texts(), seed_count + 1, "seeds and the first prompt");

        let model = seeds.model_for(counting.clone()).await;
        for prompt in ["ok thanks", "compare the tradeoff, what do you recommend"] {
            assert_eq!(
                prompt_situation(prompt, &config, &model).await,
                prompt_situation(prompt, &config, &uncached).await,
                "{prompt}"
            );
        }
        assert_eq!(texts(), seed_count + 3, "only the prompts after the first");
    }

    #[tokio::test]
    async fn seed_cache_is_bounded_and_skips_failed_builds() {
        let seeds = SeedCache::default();
        let failing = std::sync::Arc::new(FamilyEmbedder { fail: true });
        assert!(matches!(
            seeds.model_for(failing).await,
            SituationModel::Provider(_)
        ));
        assert_eq!(seeds.lock().len(), 0, "a failed build is not cached");
        for i in 0..(SEED_CACHE_MAX + 2) {
            let p = NamedFamily(format!("family-{i}"));
            assert!(seeds.classifier_for(&p).await.is_some());
        }
        assert_eq!(seeds.lock().len(), SEED_CACHE_MAX);
    }

    /// [`FamilyEmbedder`] under another model id.
    struct NamedFamily(String);

    #[async_trait::async_trait]
    impl engramdb::embeddings::EmbeddingProvider for NamedFamily {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            FamilyEmbedder { fail: false }.embed(text).await
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            FamilyEmbedder { fail: false }.embed_batch(texts).await
        }
        fn dimensions(&self) -> usize {
            SITUATION_FAMILIES.len() + 1
        }
        fn max_tokens(&self) -> usize {
            512
        }
        fn model_id(&self) -> String {
            self.0.clone()
        }
    }

    #[test]
    fn text_tail_keeps_whole_words() {
        assert_eq!(text_tail("  short  ", 300), "short");
        assert_eq!(text_tail("alpha beta gamma", 10), "beta gamma");
        // A cut inside "beta" drops the partial word.
        assert_eq!(text_tail("alpha beta gamma", 8), "gamma");
        // The cut falls exactly on a word start: nothing is dropped.
        assert_eq!(text_tail("alpha beta gamma", 5), "gamma");
        // Multibyte text is cut on a char boundary.
        assert_eq!(text_tail("über straße länge", 6), "länge");
        // One long word longer than the limit is kept as its tail.
        assert_eq!(text_tail("abcdefghij", 4), "ghij");
    }

    /// A transcript whose last assistant message is `last`.
    fn transcript_with(dir: &Path, last: &str) -> std::path::PathBuf {
        let path = dir.join("transcript.jsonl");
        let record = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": last}]}
        });
        std::fs::write(&path, format!("{record}\n")).unwrap();
        path
    }

    #[tokio::test]
    async fn user_prompt_submit_searches_the_previous_message_for_short_replies() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mut push = Memory::new(
            MemoryType::Convention,
            "Never push to main directly; open a pull request",
            "Branch protection rejects direct pushes.",
            Provenance::human(),
        );
        push.criticality = 0.9;
        store.create(&push).await.unwrap();
        let mut migration = Memory::new(
            MemoryType::Convention,
            "Schema migrations need a version bump",
            "Bump CURRENT_SCHEMA_VERSION with every new column.",
            Provenance::human(),
        );
        migration.criticality = 0.9;
        store.create(&migration).await.unwrap();

        let transcript = transcript_with(
            temp_dir.path(),
            "I committed the fix on the feature branch. Should I push it?",
        );
        let input = |prompt: &str| {
            serde_json::json!({
                "prompt": prompt,
                "transcript_path": transcript.to_str().unwrap(),
            })
            .to_string()
        };

        // "yes" matches nothing; the previous message finds the push memory.
        let out = process_user_prompt_submit(&input("yes"), temp_dir.path())
            .await
            .unwrap()
            .expect("the previous message should surface a memory");
        assert!(out.contains("Never push to main"), "{out}");

        // The prompt's own match ranks before the previous message's.
        forget_shown(&input("x"), temp_dir.path());
        let out = process_user_prompt_submit(
            &input("does this need a schema migration?"),
            temp_dir.path(),
        )
        .await
        .unwrap()
        .expect("both memories should surface");
        let (own, previous) = (out.find("Schema migrations"), out.find("Never push"));
        assert!(own.is_some() && previous.is_some(), "{out}");
        assert!(own < previous, "prompt match must rank first: {out}");

        // `previous_message_chars = 0` turns the second search off.
        forget_shown(&input("x"), temp_dir.path());
        std::fs::write(
            temp_dir.path().join(".engramdb/config.toml"),
            "[hooks]\nprevious_message_chars = 0\n",
        )
        .unwrap();
        assert!(process_user_prompt_submit(&input("yes"), temp_dir.path())
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
        // The body and the id reach the hook through a real store. Hooks once
        // asked for DetailLevel::Summary, the engine cleared `content`, and
        // the preview rendered as an empty line.
        assert!(
            ctx.contains("never cargo test"),
            "body preview missing: {ctx}"
        );
        assert!(
            ctx.contains(&format!("id: {}", mem.id)),
            "id missing: {ctx}"
        );
        assert!(ctx.contains(PREVIEW_TRUST_NOTE), "{ctx}");

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
                        Some(HookCommand::UserPromptSubmit) => "UserPromptSubmit",
                        Some(HookCommand::PostToolUse) => "PostToolUse",
                        Some(HookCommand::SessionEnd) => "SessionEnd",
                        Some(HookCommand::PreCompact) => "PreCompact",
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

    /// PreToolUse picks by nearest enclosing scope: a directory's own
    /// conventions beat a higher-scoring memory about a sibling file and a
    /// project-wide hazard, which used to take every slot.
    #[tokio::test]
    async fn test_pre_tool_use_keeps_directory_scope_over_siblings_and_ancestors() {
        let temp_dir = TempDir::new().unwrap();
        let api = temp_dir.path().join("src/api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("orders.rs"), "").unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mk = |t: MemoryType, title: &str, scope: &str, crit: f64| {
            let mut m = Memory::new(t, title, "body text", Provenance::human());
            m.physical = vec![scope.to_string()];
            m.criticality = crit;
            m
        };
        store
            .create(&mk(
                MemoryType::Hazard,
                "Sibling hazard",
                "src/api/other.rs",
                0.9,
            ))
            .await
            .unwrap();
        store
            .create(&mk(MemoryType::Hazard, "Project-wide hazard", "src/", 0.9))
            .await
            .unwrap();
        for i in 1..=5 {
            store
                .create(&mk(
                    MemoryType::Convention,
                    &format!("Dir rule {i}"),
                    "src/api/",
                    0.5,
                ))
                .await
                .unwrap();
        }
        let input = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": api.join("orders.rs").to_str().unwrap() }
        })
        .to_string();
        let out = process_hook_input(&input, temp_dir.path(), store)
            .await
            .unwrap()
            .expect("the directory rules are injected");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        for i in 1..=5 {
            assert!(ctx.contains(&format!("Dir rule {i}")), "{ctx}");
        }
        assert!(!ctx.contains("Sibling hazard"), "{ctx}");
        assert!(!ctx.contains("Project-wide hazard"), "{ctx}");
    }

    /// Within one session the file hook injects each memory once: the
    /// conversation keeps it, and a repeat is re-read on every later turn.
    /// A new session, or the same one after compaction, sees it again.
    #[tokio::test]
    async fn test_pre_tool_use_injects_each_memory_once_per_session() {
        let (temp_dir, _store) = setup_store_with_memories().await;
        let dir = temp_dir.path();
        let input = |session: &str| {
            serde_json::json!({
                "session_id": session,
                "tool_name": "Read",
                "tool_input": { "file_path": dir.join("src/main.rs").to_str().unwrap() }
            })
            .to_string()
        };
        let run = |input: String| async move {
            let store = MemoryStore::open(dir).await.unwrap();
            process_hook_input(&input, dir, store).await.unwrap()
        };
        let first = run(input("sess-a")).await.expect("first edit injects");
        assert!(first.contains("Use async everywhere"), "{first}");
        assert!(
            run(input("sess-a")).await.is_none(),
            "already shown in sess-a"
        );
        let other = run(input("sess-b")).await.expect("another session");
        assert!(other.contains("Use async everywhere"), "{other}");
        forget_shown(&input("sess-a"), dir);
        let again = run(input("sess-a")).await.expect("after compaction");
        assert!(again.contains("Use async everywhere"), "{again}");
    }

    /// The engine skips loading the already-shown memories (`skip_load`)
    /// but they keep their place in the pool, so each edit injects exactly
    /// what dropping them after the query did: the next memories in order,
    /// never one the session has, until none are left.
    #[tokio::test]
    async fn pre_tool_use_injects_the_same_memories_with_skip_load() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        let store = MemoryStore::init(dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        for i in 0..(PRE_TOOL_USE_MAX_RESULTS + 3) {
            let mut m = Memory::new(
                MemoryType::Decision,
                format!("Main rule number {i}"),
                "applies to the main file",
                Provenance::human(),
            );
            m.physical = vec!["src/main.rs".to_string()];
            m.criticality = 0.5 + i as f64 * 0.01;
            store.create(&m).await.unwrap();
        }
        let input = serde_json::json!({
            "session_id": "sess-skip",
            "tool_name": "Read",
            "tool_input": { "file_path": dir.join("src/main.rs").to_str().unwrap() }
        })
        .to_string();
        let shown_in = |out: &str| -> Vec<usize> {
            (0..(PRE_TOOL_USE_MAX_RESULTS + 3))
                .filter(|i| out.contains(&format!("Main rule number {i}")))
                .collect()
        };
        let run = || async {
            let store = MemoryStore::open(dir).await.unwrap();
            process_hook_input(&input, dir, store).await.unwrap()
        };
        let first = shown_in(&run().await.expect("first edit injects"));
        assert_eq!(first.len(), PRE_TOOL_USE_MAX_RESULTS, "{first:?}");
        let second = shown_in(&run().await.expect("the rest"));
        assert_eq!(second.len(), 3, "{second:?}");
        assert!(
            second.iter().all(|i| !first.contains(i)),
            "{first:?} {second:?}"
        );
        assert!(run().await.is_none(), "everything was shown");
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
                Some(HookCommand::PreToolUse) => {} // expected
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
                Some(HookCommand::PreToolUse) => {}
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
                Some(HookCommand::SessionStart { min_criticality }) => {
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
                Some(HookCommand::SessionStart { min_criticality }) => {
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

    /// The SessionStart context, from the hook's JSON.
    fn session_start_context(out: &str) -> String {
        let parsed: serde_json::Value = serde_json::from_str(out).unwrap();
        parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn session_start_registers_an_unregistered_store_and_says_so() {
        // A project on a removable drive, set up against another registry.
        let temp_dir =
            store_with_criticality(&[(0.9, "Use the drive"), (0.8, "Keep it dry")]).await;
        let registry = InMemoryRegistry::new();

        let out = process_session_start_in(
            temp_dir.path(),
            0.5,
            "",
            &StoreSource::Fresh,
            &UsageSink::Log,
            Some(&registry),
        )
        .await
        .unwrap()
        .expect("SessionStart still emits context");
        let ctx = session_start_context(&out);
        assert!(
            ctx.starts_with("[EngramDB] This project ("),
            "the notice leads the context: {ctx}"
        );
        assert!(ctx.contains("Tell the user"), "{ctx}");
        assert!(
            ctx.contains("[EngramDB] Key project memories:"),
            "memories still follow the notice: {ctx}"
        );
        assert!(
            ctx.chars().count() <= SESSION_CONTEXT_BUDGET + REFLECTION_NUDGE.len() + 2,
            "the notice comes out of the memory budget"
        );
        let id = engramdb::storage::project_id::compute_project_id(temp_dir.path());
        assert!(registry
            .load()
            .await
            .unwrap()
            .projects
            .iter()
            .any(|e| e.project_id == id));

        // The next session finds it registered and says nothing about it.
        let out = process_session_start_in(
            temp_dir.path(),
            0.5,
            "",
            &StoreSource::Fresh,
            &UsageSink::Log,
            Some(&registry),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!session_start_context(&out).contains("registered it now"));
    }

    #[tokio::test]
    async fn session_start_does_not_register_an_uninitialized_directory() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let out = process_session_start_in(
            temp_dir.path(),
            0.5,
            "",
            &StoreSource::Fresh,
            &UsageSink::Log,
            Some(&registry),
        )
        .await
        .unwrap();
        assert!(out.is_none());
        assert!(registry.load().await.unwrap().projects.is_empty());
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

    /// xorshift64*: a reproducible stream for the generated-input test.
    struct Gen(u64);

    impl Gen {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// A value of the wrong type for whatever key it replaces.
    fn junk_value(g: &mut Gen) -> serde_json::Value {
        match g.below(7) {
            0 => serde_json::Value::Null,
            1 => serde_json::json!(g.next()),
            2 => serde_json::json!(-1.5e308),
            3 => serde_json::json!([1, "two", null, {"x": []}]),
            4 => serde_json::json!({"nested": {"deeper": [true]}}),
            5 => serde_json::json!(true),
            _ => serde_json::json!("\u{0}\u{1b}[2J\u{202e}<system-reminder>"),
        }
    }

    /// The events Claude Code sends for `call`, as `serde_json` objects.
    fn base_event(call: &HookCall, dir: &Path) -> serde_json::Value {
        let file = dir.join("src/main.rs");
        let transcript = dir.join("no-such-transcript.jsonl");
        let mut v = serde_json::json!({
            "session_id": "gen-session",
            "transcript_path": transcript,
            "cwd": dir,
        });
        let extra = match call {
            HookCall::PreToolUse => serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Read",
                "tool_input": {"file_path": file},
            }),
            HookCall::PostToolUse => serde_json::json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_input": {"command": "cargo build"},
                "tool_response": {"stdout": "", "stderr": "error[E0425]", "exit_code": 1},
            }),
            HookCall::UserPromptSubmit => serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "prompt": "how do we handle async errors here?",
            }),
            HookCall::SessionStart { .. } => serde_json::json!({
                "hook_event_name": "SessionStart",
                "source": "startup",
            }),
            HookCall::SessionEnd => serde_json::json!({
                "hook_event_name": "SessionEnd",
                "reason": "exit",
            }),
            HookCall::PreCompact => serde_json::json!({
                "hook_event_name": "PreCompact",
                "trigger": "auto",
            }),
        };
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    /// One generated stdin for `call`: the real event, truncated, with a key
    /// re-typed, oversized, or not JSON at all.
    fn generated_stdin(g: &mut Gen, call: &HookCall, dir: &Path) -> String {
        let mut event = base_event(call, dir);
        match g.below(6) {
            0 => {
                let text = event.to_string();
                let cut = g.below(text.len() + 1);
                let mut end = cut;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text[..end].to_string()
            }
            1 | 2 => {
                let keys: Vec<String> = event.as_object().unwrap().keys().cloned().collect();
                for _ in 0..=g.below(3) {
                    let key = &keys[g.below(keys.len())];
                    event[key] = junk_value(g);
                }
                event.to_string()
            }
            3 => {
                let keys: Vec<String> = event.as_object().unwrap().keys().cloned().collect();
                let key = &keys[g.below(keys.len())];
                event[key] = serde_json::json!("x".repeat(256 * 1024));
                event.to_string()
            }
            4 => {
                let keys: Vec<String> = event.as_object().unwrap().keys().cloned().collect();
                event
                    .as_object_mut()
                    .unwrap()
                    .remove(&keys[g.below(keys.len())]);
                event.to_string()
            }
            _ => {
                let len = g.below(200);
                let bytes: Vec<u8> = (0..len).map(|_| g.next() as u8).collect();
                String::from_utf8_lossy(&bytes).into_owned()
            }
        }
    }

    /// Every hook fails open on whatever stdin it is handed: truncated,
    /// mis-typed, oversized or not JSON. No panic (an abort under the release
    /// profile, which Claude Code reports as a broken hook), and any output is
    /// the JSON document Claude Code parses.
    #[tokio::test]
    async fn hook_handlers_survive_generated_stdin() {
        let (tmp, _store) = setup_store_with_memories().await;
        let env = HookEnv {
            stores: StoreSource::Fresh,
            situation: SituationModel::KeywordOnly,
            usage: UsageSink::Log,
            registry: None,
        };
        let calls = [
            HookCall::PreToolUse,
            HookCall::SessionStart {
                min_criticality: 0.5,
            },
            HookCall::UserPromptSubmit,
            HookCall::PostToolUse,
            HookCall::SessionEnd,
            HookCall::PreCompact,
        ];
        let mut g = Gen(0x5eed_0ff0_0dca_fe00);
        for call in &calls {
            for case in 0..25 {
                let stdin = generated_stdin(&mut g, call, tmp.path());
                let preview: String = stdin.chars().take(200).collect();
                // `Ok(None)` (nothing to inject) and `Err` (swallowed by the
                // caller) are both fail-open; only an output is checked.
                if let Ok(Some(out)) = execute_hook(call, tmp.path(), &stdin, &env).await {
                    assert!(
                        serde_json::from_str::<serde_json::Value>(&out).is_ok(),
                        "{} case {case}: output is not JSON: {out}\nstdin: {preview}",
                        call.name()
                    );
                }
            }
        }
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

#[cfg(test)]
mod daemon_executor_tests {
    use super::*;
    use engramdb::storage::InMemoryRegistry;
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    /// A store with two memories scoped to `src/main.rs`, which exists.
    async fn setup_store_with_memories() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "").unwrap();
        // Registered in this test process's own registry (the `#[ctor]` arm
        // points it at a temp dir), like a real project: the daemon path
        // checks SessionStart's project against it.
        let store = MemoryStore::init(
            tmp.path(),
            &engramdb::storage::FileRegistry::global().unwrap(),
        )
        .await
        .unwrap();
        for (type_, summary, crit) in [
            (MemoryType::Decision, "Use async everywhere", 0.9),
            (MemoryType::Hazard, "Avoid blocking calls in async", 0.8),
        ] {
            let mut mem = Memory::new(type_, summary, "Details.", Provenance::human());
            mem.physical = vec!["src/main.rs".to_string()];
            mem.criticality = crit;
            store.create(&mem).await.unwrap();
        }
        let store = MemoryStore::open(tmp.path()).await.unwrap();
        (tmp, store)
    }

    // ---- the daemon executor ---------------------------------------------------

    fn read_event(path: &Path, session: &str) -> String {
        serde_json::json!({
            "tool_name": "Read",
            "tool_input": { "file_path": path },
            "session_id": session,
        })
        .to_string()
    }

    async fn daemon_run(
        exec: &DaemonHookExecutor,
        call: &HookCall,
        dir: &Path,
        input: &str,
    ) -> engramdb::daemon::HookRun {
        use engramdb::daemon::HookExecutor;
        let models = engramdb::ops::ProviderCache::new();
        exec.run(call, dir, input, &models).await
    }

    async fn in_process_run(call: &HookCall, dir: &Path, input: &str) -> Option<String> {
        let env = HookEnv {
            stores: StoreSource::Fresh,
            situation: SituationModel::KeywordOnly,
            usage: UsageSink::Log,
            registry: None,
        };
        execute_hook(call, dir, input, &env).await.unwrap()
    }

    fn logged_usage(dir: &Path) -> Vec<engramdb::ops::usage::MemoryEvent> {
        engramdb::telemetry::usage::load_memory_events(dir)
            .unwrap()
            .events
    }

    /// In its own process a hook appends `injected` — and nothing else — to
    /// the JSONL log, one event per memory it rendered, under its own
    /// surface; in the daemon the same events come back in the run for the
    /// daemon to buffer, and nothing touches the log.
    #[tokio::test]
    async fn hooks_record_only_injected_on_both_paths() {
        let (tmp, store) = setup_store_with_memories().await;
        let file = tmp.path().join("src/main.rs");
        let here =
            in_process_run(&HookCall::PreToolUse, tmp.path(), &read_event(&file, "u1")).await;
        assert!(here.is_some());
        let logged = logged_usage(tmp.path());
        assert_eq!(logged.len(), 2, "{logged:?}");
        assert!(logged
            .iter()
            .all(|e| e.kind == engramdb::ops::UsageKind::Injected
                && e.surface == Surface::HookPreTool
                && e.store_id == store.project_id
                && e.session_id.as_deref() == Some("u1")));
        assert_eq!(logged[0].rank, Some(1));

        in_process_run(
            &HookCall::SessionStart {
                min_criticality: 0.5,
            },
            tmp.path(),
            "{\"session_id\":\"u2\"}",
        )
        .await;
        let logged = logged_usage(tmp.path());
        assert!(logged[2..]
            .iter()
            .all(|e| e.surface == Surface::HookSessionStart));

        let exec = DaemonHookExecutor::new().unwrap();
        let before = logged_usage(tmp.path()).len();
        let run = daemon_run(
            &exec,
            &HookCall::PreToolUse,
            tmp.path(),
            &read_event(&file, "u3"),
        )
        .await;
        assert_eq!(run.usage_events.len(), 2);
        assert!(run
            .usage_events
            .iter()
            .all(|e| e.surface == Surface::HookPreTool));
        assert_eq!(
            logged_usage(tmp.path()).len(),
            before,
            "the daemon path writes no JSONL"
        );

        // A second Read in the same session shows nothing new: no event.
        let again = daemon_run(
            &exec,
            &HookCall::PreToolUse,
            tmp.path(),
            &read_event(&file, "u3"),
        )
        .await;
        assert!(again.usage_events.is_empty());
    }

    #[tokio::test]
    async fn hooks_record_no_usage_with_stats_disabled() {
        let (tmp, _store) = setup_store_with_memories().await;
        let config = tmp.path().join(".engramdb/config.toml");
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        std::fs::write(&config, format!("{text}\n[stats]\nenabled = false\n")).unwrap();
        let file = tmp.path().join("src/main.rs");
        assert!(
            in_process_run(&HookCall::PreToolUse, tmp.path(), &read_event(&file, "d1"))
                .await
                .is_some()
        );
        assert!(logged_usage(tmp.path()).is_empty());
    }

    /// The daemon prints exactly what the hook's own process prints. Distinct
    /// session ids, so the "already shown" record cannot hide anything.
    #[tokio::test]
    async fn daemon_executor_output_matches_in_process() {
        let (tmp, _store) = setup_store_with_memories().await;
        let file = tmp.path().join("src/main.rs");
        let exec = DaemonHookExecutor::new().unwrap();
        for (call, input_a, input_b) in [
            (
                HookCall::PreToolUse,
                read_event(&file, "a1"),
                read_event(&file, "b1"),
            ),
            (
                HookCall::SessionStart {
                    min_criticality: 0.5,
                },
                "{\"session_id\":\"a2\"}".to_string(),
                "{\"session_id\":\"b2\"}".to_string(),
            ),
            (
                HookCall::PreCompact,
                "{\"session_id\":\"a3\"}".to_string(),
                "{\"session_id\":\"b3\"}".to_string(),
            ),
        ] {
            let here = in_process_run(&call, tmp.path(), &input_a).await;
            let run = daemon_run(&exec, &call, tmp.path(), &input_b).await;
            assert!(here.is_some(), "{} printed nothing", call.name());
            assert_eq!(run.stdout, here, "{} differs", call.name());
            assert!(run.ok);
        }
    }

    /// The daemon path runs the SessionStart registry check too.
    #[tokio::test]
    async fn daemon_executor_registers_an_unregistered_store() {
        let tmp = TempDir::new().unwrap();
        MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let exec = DaemonHookExecutor::new().unwrap();
        let call = HookCall::SessionStart {
            min_criticality: 0.5,
        };
        let run = daemon_run(&exec, &call, tmp.path(), "{\"session_id\":\"r1\"}").await;
        let out = run.stdout.expect("SessionStart printed");
        assert!(out.contains("registered it now"), "{out}");
        let id = engramdb::storage::project_id::compute_project_id(tmp.path());
        let reg = engramdb::storage::FileRegistry::global()
            .unwrap()
            .load()
            .await
            .unwrap();
        assert!(reg.projects.iter().any(|e| e.project_id == id));
    }

    /// A project that fell out of the registry may have lost its data
    /// directory to the orphan sweep: the notice then says to reindex.
    #[tokio::test]
    async fn registration_notice_names_reindex_when_the_index_is_empty() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = engramdb::types::Memory::new(
            MemoryType::Decision,
            "T",
            "C",
            engramdb::types::Provenance::human(),
        );
        store.create(&mem).await.unwrap();
        let lancedb = engramdb::storage::paths::lancedb_dir(&store.project_id).unwrap();
        drop(store);
        std::fs::remove_dir_all(lancedb).unwrap();

        let registry = InMemoryRegistry::new();
        let notice = registration_notice(tmp.path(), Some(&registry))
            .await
            .expect("an unregistered store gets a notice");
        assert!(notice.contains("registered it now"), "{notice}");
        assert!(notice.contains("`engramdb reindex`"), "{notice}");
        assert!(notice.contains("1 memory"), "{notice}");
    }

    /// A store the daemon has cached still sees a memory written afterwards by
    /// another process (every LanceDB call reopens its table).
    #[tokio::test]
    async fn daemon_executor_sees_writes_made_after_caching() {
        let (tmp, store) = setup_store_with_memories().await;
        let file = tmp.path().join("src/main.rs");
        let exec = DaemonHookExecutor::new().unwrap();
        let first = daemon_run(
            &exec,
            &HookCall::PreToolUse,
            tmp.path(),
            &read_event(&file, "f1"),
        )
        .await;
        assert!(!first.stdout.unwrap().contains("Freshly written rule"));
        assert_eq!(exec.stores.len().await, 1);

        let mut mem = Memory::new(
            MemoryType::Hazard,
            "Freshly written rule",
            "Written while the daemon held a warm store",
            Provenance::human(),
        );
        mem.physical = vec!["src/main.rs".to_string()];
        mem.criticality = 0.99;
        store.create(&mem).await.unwrap();

        let second = daemon_run(
            &exec,
            &HookCall::PreToolUse,
            tmp.path(),
            &read_event(&file, "f2"),
        )
        .await;
        assert!(
            second.stdout.unwrap().contains("Freshly written rule"),
            "the warm store must not serve a stale table version"
        );
    }

    #[tokio::test]
    async fn store_cache_reopens_on_config_change_and_refuses_a_removed_store() {
        let (tmp, _store) = setup_store_with_memories().await;
        let cache = StoreCache::default();
        cache.get_or_open(tmp.path()).await.unwrap();
        let stamp = store_stamp(tmp.path());
        // A config edit changes the stamp, so the next call reopens.
        let config = tmp.path().join(".engramdb/config.toml");
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&config, format!("{text}\n")).unwrap();
        assert_ne!(store_stamp(tmp.path()), stamp);
        cache.get_or_open(tmp.path()).await.unwrap();
        assert_eq!(cache.len().await, 1, "reopened in place, not duplicated");

        std::fs::remove_dir_all(tmp.path().join(".engramdb")).unwrap();
        assert!(cache.get_or_open(tmp.path()).await.is_err());
        assert!(
            cache.is_empty().await,
            "a removed store is dropped from the cache"
        );
    }

    #[tokio::test]
    async fn daemon_executor_reports_a_project_only_when_initialized() {
        let exec = DaemonHookExecutor::new().unwrap();
        let bare = TempDir::new().unwrap();
        let run = daemon_run(&exec, &HookCall::PreCompact, bare.path(), "{}").await;
        assert!(run.project.is_none(), "an un-init'ed dir collects no stats");
        assert!(run.stdout.is_some(), "the reminder still prints");

        let (tmp, store) = setup_store_with_memories().await;
        let run = daemon_run(
            &exec,
            &HookCall::PreCompact,
            tmp.path(),
            "{\"session_id\":\"sess-9\"}",
        )
        .await;
        let project = run.project.expect("initialized store");
        assert_eq!(project.project_id, store.project_id);
        assert_eq!(project.session_id.as_deref(), Some("sess-9"));
        assert!(project.stats_enabled);
    }

    /// A daemon-served UserPromptSubmit in a directory with no store returns
    /// nothing, so it must not load (or start loading) a model either: it
    /// used to load the config's whole embedding pool before looking.
    #[tokio::test]
    async fn daemon_user_prompt_submit_in_an_uninitialized_dir_touches_no_model() {
        use engramdb::daemon::HookExecutor;
        let exec = DaemonHookExecutor::new().unwrap();
        let bare = TempDir::new().unwrap();
        let models = engramdb::ops::ProviderCache::new();
        let input = serde_json::json!({ "prompt": "why does the build fail?" }).to_string();
        let run = exec
            .run(&HookCall::UserPromptSubmit, bare.path(), &input, &models)
            .await;
        assert!(run.project.is_none());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(models.loaded_count().await, 0, "a model was loaded");
        assert!(models.model_ids().await.is_empty());
    }

    /// stdin is read through a cap: an over-cap event is cut (and flagged),
    /// never read without bound into memory.
    #[test]
    fn read_capped_truncates_at_the_cap() {
        let (text, capped) = read_capped(std::io::Cursor::new(b"abcdef".to_vec()), 4);
        assert_eq!(text, "abcd");
        assert!(capped);
        let (text, capped) = read_capped(std::io::Cursor::new(b"abcd".to_vec()), 4);
        assert_eq!(text, "abcd");
        assert!(!capped, "exactly the cap is not over it");
    }
}

/// `delegate_to_daemon`'s mapping from what the daemon did to what the CLI
/// does next. The socket is steered through the armed (per-process) global
/// config, so no environment variable is touched.
#[cfg(test)]
mod delegation_tests {
    use super::*;
    use engramdb::daemon::protocol::{DaemonOp, DaemonResponse};
    use engramdb::daemon::test_support::{current_pong, spawn_recording_stub, wait_connectable};

    /// A 0700 temp dir: the transport refuses a daemon folder with group or
    /// other bits, and `TempDir::new` follows the umask.
    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    /// Point the global `[daemon]` at `socket`.
    fn configure(socket: &Path, enabled: bool, use_for_cli: bool) {
        let path = engramdb::storage::paths::global_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "[daemon]\nenabled = {enabled}\nuse_for_cli = {use_for_cli}\nsocket_path = {:?}\n",
                socket.display().to_string()
            ),
        )
        .unwrap();
    }

    fn ops(log: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        log.lock().unwrap().clone()
    }

    fn sent_hook(ops: &[String]) -> bool {
        ops.iter().any(|op| op.starts_with("Hook"))
    }

    const EVENT: &str = r#"{"session_id":"s1"}"#;

    #[tokio::test]
    async fn hook_lost_reply_is_done_and_runs_nothing_here() {
        let sock_dir = private_dir();
        let socket = sock_dir.path().join("d.sock");
        let log = spawn_recording_stub(socket.clone(), |op| match op {
            DaemonOp::Version => Some(current_pong()),
            // The daemon took the hook and died before answering.
            _ => None,
        });
        wait_connectable(&socket).await;
        configure(&socket, true, true);
        let project = tempfile::TempDir::new().unwrap();

        let out = delegate_to_daemon(&HookCall::PreCompact, project.path(), EVENT).await;

        assert!(
            matches!(out, Delegated::Done),
            "a lost reply must not run the hook again here"
        );
        assert!(
            sent_hook(&ops(&log)),
            "the hook was never sent: {:?}",
            ops(&log)
        );
    }

    #[tokio::test]
    async fn hook_not_sent_runs_here() {
        let sock_dir = private_dir();
        configure(&sock_dir.path().join("nobody.sock"), true, true);
        let project = tempfile::TempDir::new().unwrap();
        let out = delegate_to_daemon(&HookCall::PreCompact, project.path(), EVENT).await;
        assert!(matches!(out, Delegated::RunHere));
    }

    #[tokio::test]
    async fn hook_to_an_older_protocol_daemon_runs_here() {
        let sock_dir = private_dir();
        let socket = sock_dir.path().join("d.sock");
        let older = (engramdb::daemon::PROTOCOL_VERSION.parse::<i64>().unwrap() - 1).to_string();
        let log = spawn_recording_stub(socket.clone(), move |op| match op {
            DaemonOp::Version | DaemonOp::Ping => Some(DaemonResponse::Pong {
                version: older.clone(),
                build: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            _ => None,
        });
        wait_connectable(&socket).await;
        configure(&socket, true, true);
        let project = tempfile::TempDir::new().unwrap();

        let out = delegate_to_daemon(&HookCall::PreCompact, project.path(), EVENT).await;

        assert!(matches!(out, Delegated::RunHere));
        assert!(
            !sent_hook(&ops(&log)),
            "a hook was sent to an older protocol: {:?}",
            ops(&log)
        );
    }

    async fn never_connects(enabled: bool, use_for_cli: bool) {
        let sock_dir = private_dir();
        let socket = sock_dir.path().join("d.sock");
        let log = spawn_recording_stub(socket.clone(), |op| match op {
            DaemonOp::Version | DaemonOp::Ping => Some(current_pong()),
            _ => Some(DaemonResponse::HookOutput {
                stdout: None,
                ok: true,
            }),
        });
        wait_connectable(&socket).await;
        configure(&socket, enabled, use_for_cli);
        let project = tempfile::TempDir::new().unwrap();

        let out = delegate_to_daemon(&HookCall::PreCompact, project.path(), EVENT).await;

        assert!(matches!(out, Delegated::RunHere));
        assert!(
            ops(&log).is_empty(),
            "the daemon was contacted: {:?}",
            ops(&log)
        );
    }

    #[tokio::test]
    async fn hook_use_for_cli_false_never_connects() {
        never_connects(true, false).await;
    }

    #[tokio::test]
    async fn hook_daemon_disabled_never_connects() {
        never_connects(false, true).await;
    }

    #[tokio::test]
    async fn hook_served_is_done() {
        let sock_dir = private_dir();
        let socket = sock_dir.path().join("d.sock");
        let log = spawn_recording_stub(socket.clone(), |op| match op {
            DaemonOp::Version | DaemonOp::Ping => Some(current_pong()),
            DaemonOp::Hook { .. } => Some(DaemonResponse::HookOutput {
                stdout: None,
                ok: true,
            }),
            _ => None,
        });
        wait_connectable(&socket).await;
        configure(&socket, true, true);
        let project = tempfile::TempDir::new().unwrap();

        let out = delegate_to_daemon(&HookCall::PreCompact, project.path(), EVENT).await;

        assert!(matches!(out, Delegated::Done));
        assert!(sent_hook(&ops(&log)));
    }
}
