//! Set up Claude Code integration for the current project.

use crate::cli::output::OutputFormatter;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGRAM_MD_CONTENT: &str = r#"# EngramDB

This project uses EngramDB for persistent agent memory.

- **Expand surfaced memories** — when memories are surfaced at session start, `get` the full content of any relevant to the current task before proceeding.
- **Search before answering** — call `search` before answering any question that touches project knowledge, whether real or hypothetical.
- **Retrieve before modifying** — call `retrieve` with the file path before modifying files, to surface known decisions, hazards, or conventions.
- **Store after discovering** — call `create` after discovering important patterns, decisions, hazards, or conventions worth preserving.
- **Challenge contradictions** — call `challenge` when you find information that contradicts an existing memory.
"#;

const ENGRAM_MD_REF: &str = "@ENGRAM.md";

/// MCP tool suffixes that need permission entries.
const MCP_TOOL_SUFFIXES: &[&str] = &[
    "search",
    "retrieve",
    "create",
    "get",
    "list",
    "update",
    "delete",
    "challenge",
    "resolve",
    "review",
    "stats",
    "doctor",
    "gc",
    "reindex",
    "compress_candidates",
    "compress_apply",
];

/// Tool prefix when engramdb is installed as a Claude Code plugin.
const PLUGIN_MCP_PREFIX: &str = "mcp__plugin_engram_memory__";

/// Tool prefix when engramdb MCP is configured in settings.json.
const SETTINGS_MCP_PREFIX: &str = "mcp__engramdb__";

/// Resolve the base directory for `.claude/` files and `ENGRAM.md`.
///
/// - Default: `<project>/.claude/` for settings, `<project>/ENGRAM.md` for directives
/// - `--global`: `~/.claude/` for everything
/// - `--claude-dir <path>`: custom override (useful for testing)
fn resolve_claude_dir(
    project_dir: &Path,
    global: bool,
    claude_dir_override: Option<&Path>,
) -> PathBuf {
    if let Some(override_dir) = claude_dir_override {
        return override_dir.to_path_buf();
    }
    if global {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
    } else {
        project_dir.join(".claude")
    }
}

/// Run the setup command.
///
/// Configures Claude Code integration by:
/// 1. Installing the engramdb plugin (or falling back to hooks + MCP in settings.json)
/// 2. Writing ENGRAM.md with agent directives
/// 3. Adding @ENGRAM.md reference to CLAUDE.md
pub async fn run_setup(
    project_dir: &Path,
    no_plugin: bool,
    global: bool,
    dry_run: bool,
    claude_dir_override: Option<&Path>,
    formatter: &OutputFormatter,
) -> Result<()> {
    run_setup_inner(
        project_dir,
        no_plugin,
        global,
        dry_run,
        claude_dir_override,
        None,
        formatter,
    )
    .await
}

async fn run_setup_inner(
    project_dir: &Path,
    no_plugin: bool,
    global: bool,
    dry_run: bool,
    claude_dir_override: Option<&Path>,
    plugins_dir_override: Option<&Path>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let claude_dir = resolve_claude_dir(project_dir, global, claude_dir_override);
    let mut any_changes = false;

    // Step 1: Plugin install (or hooks + MCP fallback)
    // If the plugin is already installed globally, skip hooks/MCP in all modes
    // to avoid duplicate hooks and MCP servers.
    let plugin_active = if is_plugin_installed_in(plugins_dir_override) {
        formatter.print_message("Plugin already installed — skipping hooks and MCP setup.");
        true
    } else if global && !no_plugin {
        let plugin_installed = try_install_plugin(dry_run, formatter);
        if !plugin_installed {
            any_changes |= install_settings_fallback(&claude_dir, dry_run, formatter)?;
            false
        } else {
            any_changes = true;
            true
        }
    } else {
        any_changes |= install_settings_fallback(&claude_dir, dry_run, formatter)?;
        false
    };

    // Step 1b: Ensure MCP tool permissions
    let mcp_prefix = if plugin_active {
        PLUGIN_MCP_PREFIX
    } else {
        SETTINGS_MCP_PREFIX
    };
    any_changes |= ensure_mcp_permissions(&claude_dir, mcp_prefix, dry_run, formatter)?;

    // Step 2: Write ENGRAM.md (same dir as CLAUDE.md so @ENGRAM.md resolves)
    any_changes |= write_engram_md(&claude_dir, dry_run, formatter)?;

    // Step 3: Update CLAUDE.md with @ENGRAM.md reference
    any_changes |= update_claude_md(&claude_dir, dry_run, formatter)?;

    if !any_changes {
        formatter.print_message("Everything is already set up. Nothing to do.");
    }

    Ok(())
}

/// Check if the engramdb plugin is already installed by reading Claude Code's
/// installed_plugins.json registry.
///
/// When `plugins_dir` is `None`, reads from `~/.claude/plugins/`.
fn is_plugin_installed_in(plugins_dir: Option<&Path>) -> bool {
    let installed_path = if let Some(dir) = plugins_dir {
        dir.join("installed_plugins.json")
    } else {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        home.join(".claude")
            .join("plugins")
            .join("installed_plugins.json")
    };
    let Ok(content) = std::fs::read_to_string(&installed_path) else {
        return false;
    };
    let Ok(data) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    let Some(plugins) = data.get("plugins").and_then(|p| p.as_object()) else {
        return false;
    };
    plugins
        .keys()
        .any(|key| key.starts_with("engramdb@") || key.starts_with("engram@"))
}

/// Try to install the engramdb plugin via `claude plugin add`.
/// Returns true if successful, false if claude CLI is unavailable or the command fails.
fn try_install_plugin(dry_run: bool, formatter: &OutputFormatter) -> bool {
    let claude_available = Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !claude_available {
        formatter.print_message("Claude CLI not found, falling back to settings.json.");
        return false;
    }

    if dry_run {
        formatter.print_message("Would install engramdb plugin via `claude plugin add`.");
        return true;
    }

    let result = Command::new("claude")
        .args(["plugin", "add", "engramdb"])
        .output();

    match result {
        Ok(output) if output.status.success() => {
            formatter.print_success("Installed engramdb plugin.");
            true
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("already") {
                formatter.print_message("Plugin already installed.");
                true
            } else {
                formatter.print_message("Plugin install failed, falling back to settings.json.");
                false
            }
        }
        Err(_) => {
            formatter.print_message("Plugin install failed, falling back to settings.json.");
            false
        }
    }
}

/// Write hooks and MCP server config into settings.json (merge strategy).
fn install_settings_fallback(
    claude_dir: &Path,
    dry_run: bool,
    formatter: &OutputFormatter,
) -> Result<bool> {
    let settings_path = claude_dir.join("settings.json");

    let mut settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    let mut changed = false;

    // --- Hooks ---
    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));

    changed |= ensure_hook_entry(
        hooks,
        "PreToolUse",
        json!({
            "matcher": "Read|Write|Edit",
            "hooks": [{
                "type": "command",
                "command": "engramdb hook pre-tool-use --dir ."
            }]
        }),
        "engramdb hook pre-tool-use",
    );

    changed |= ensure_hook_entry(
        hooks,
        "SessionStart",
        json!({
            "hooks": [{
                "type": "command",
                "command": "engramdb hook session-start --dir ."
            }]
        }),
        "engramdb hook session-start",
    );

    // --- MCP server ---
    changed |= ensure_mcp_server(&mut settings);

    if !changed {
        formatter.print_message("Hooks and MCP already configured in settings.json.");
        return Ok(false);
    }

    if dry_run {
        formatter.print_message("Would add hooks and MCP server to settings.json.");
        return Ok(true);
    }

    std::fs::create_dir_all(claude_dir)?;
    let formatted = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, formatted)?;
    formatter.print_success("Added hooks and MCP server to settings.json.");
    Ok(true)
}

/// Ensure the engramdb MCP server entry exists in settings.json.
/// Returns true if it was added.
fn ensure_mcp_server(settings: &mut Value) -> bool {
    let mcp_servers = settings
        .as_object_mut()
        .unwrap()
        .entry("mcpServers")
        .or_insert_with(|| json!({}));

    if mcp_servers.get("engramdb").is_some() {
        return false;
    }

    mcp_servers.as_object_mut().unwrap().insert(
        "engramdb".to_string(),
        json!({
            "command": "engramdb",
            "args": ["serve", "--dir", "."]
        }),
    );
    true
}

/// Ensure a hook entry exists in the given event array, matched by command substring.
/// Returns true if a new entry was added.
fn ensure_hook_entry(hooks: &mut Value, event: &str, entry: Value, match_command: &str) -> bool {
    let event_array = hooks
        .as_object_mut()
        .unwrap()
        .entry(event)
        .or_insert_with(|| json!([]));

    let arr = event_array.as_array().unwrap();

    let already_exists = arr.iter().any(|e| {
        if let Some(inner_hooks) = e.get("hooks").and_then(|h| h.as_array()) {
            inner_hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(match_command))
                    .unwrap_or(false)
            })
        } else {
            false
        }
    });

    if already_exists {
        return false;
    }

    event_array.as_array_mut().unwrap().push(entry);
    true
}

/// Ensure MCP tool permissions are present in settings.json.
/// Uses the given prefix (plugin or settings MCP) to generate permission entries.
/// Returns true if any permissions were added.
fn ensure_mcp_permissions(
    claude_dir: &Path,
    prefix: &str,
    dry_run: bool,
    formatter: &OutputFormatter,
) -> Result<bool> {
    let settings_path = claude_dir.join("settings.json");

    let mut settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    let permissions = settings
        .as_object_mut()
        .unwrap()
        .entry("permissions")
        .or_insert_with(|| json!({}));
    let allow = permissions
        .as_object_mut()
        .unwrap()
        .entry("allow")
        .or_insert_with(|| json!([]));
    let allow_arr = allow.as_array_mut().unwrap();

    let existing: std::collections::HashSet<String> = allow_arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let mut added = false;
    for suffix in MCP_TOOL_SUFFIXES {
        let perm = format!("{prefix}{suffix}");
        if !existing.contains(&perm) {
            allow_arr.push(json!(perm));
            added = true;
        }
    }

    if !added {
        formatter.print_message("MCP permissions already configured.");
        return Ok(false);
    }

    if dry_run {
        formatter.print_message("Would add MCP tool permissions to settings.json.");
        return Ok(true);
    }

    std::fs::create_dir_all(claude_dir)?;
    let formatted = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, formatted)?;
    formatter.print_success("Added MCP tool permissions to settings.json.");
    Ok(true)
}

/// Write ENGRAM.md in the target directory.
fn write_engram_md(engram_dir: &Path, dry_run: bool, formatter: &OutputFormatter) -> Result<bool> {
    let engram_path = engram_dir.join("ENGRAM.md");

    if engram_path.exists() {
        let existing = std::fs::read_to_string(&engram_path)?;
        if existing == ENGRAM_MD_CONTENT {
            formatter.print_message("ENGRAM.md is already up to date.");
            return Ok(false);
        }
    }

    if dry_run {
        if engram_path.exists() {
            formatter.print_message("Would update ENGRAM.md.");
        } else {
            formatter.print_message("Would create ENGRAM.md.");
        }
        return Ok(true);
    }

    let is_update = engram_path.exists();
    std::fs::create_dir_all(engram_dir)?;
    std::fs::write(&engram_path, ENGRAM_MD_CONTENT)?;
    if is_update {
        formatter.print_success("Updated ENGRAM.md.");
    } else {
        formatter.print_success("Created ENGRAM.md.");
    }
    Ok(true)
}

/// Add @ENGRAM.md reference to CLAUDE.md if not already present.
fn update_claude_md(claude_dir: &Path, dry_run: bool, formatter: &OutputFormatter) -> Result<bool> {
    let claude_md_path = claude_dir.join("CLAUDE.md");

    let existing = if claude_md_path.exists() {
        std::fs::read_to_string(&claude_md_path)?
    } else {
        String::new()
    };

    if existing.lines().any(|line| line.trim() == ENGRAM_MD_REF) {
        formatter.print_message("CLAUDE.md already references ENGRAM.md.");
        return Ok(false);
    }

    if dry_run {
        if existing.is_empty() {
            formatter.print_message("Would create CLAUDE.md with @ENGRAM.md reference.");
        } else {
            formatter.print_message("Would add @ENGRAM.md reference to CLAUDE.md.");
        }
        return Ok(true);
    }

    std::fs::create_dir_all(claude_dir)?;

    let new_content = if existing.is_empty() {
        format!("{ENGRAM_MD_REF}\n")
    } else {
        let separator = if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        format!("{existing}{separator}{ENGRAM_MD_REF}\n")
    };

    std::fs::write(&claude_md_path, new_content)?;
    if existing.is_empty() {
        formatter.print_success("Created CLAUDE.md with @ENGRAM.md reference.");
    } else {
        formatter.print_success("Added @ENGRAM.md reference to CLAUDE.md.");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::app::OutputFormat;
    use tempfile::TempDir;

    fn test_formatter() -> OutputFormatter {
        OutputFormatter::new(Some(OutputFormat::Plain), false, true)
    }

    /// Create a fake plugins dir with no installed plugins (empty registry).
    fn fake_plugins_dir(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join("plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("installed_plugins.json"),
            r#"{"version":2,"plugins":{}}"#,
        )
        .unwrap();
        dir
    }

    /// Create a fake plugins dir with the engram plugin installed.
    fn fake_plugins_dir_with_engram(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join("plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("installed_plugins.json"),
            r#"{"version":2,"plugins":{"engramdb@engramdb":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        dir
    }

    // --- Plugin detection tests ---

    #[test]
    fn test_plugin_detection_finds_engramdb() {
        let tmp = TempDir::new().unwrap();
        let dir = fake_plugins_dir_with_engram(&tmp);
        assert!(is_plugin_installed_in(Some(&dir)));
    }

    #[test]
    fn test_plugin_detection_empty_registry() {
        let tmp = TempDir::new().unwrap();
        let dir = fake_plugins_dir(&tmp);
        assert!(!is_plugin_installed_in(Some(&dir)));
    }

    #[test]
    fn test_plugin_detection_missing_file() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_plugin_installed_in(Some(tmp.path())));
    }

    // --- ENGRAM.md tests ---

    #[test]
    fn test_write_engram_md_creates_file() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let changed = write_engram_md(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("ENGRAM.md")).unwrap();
        assert_eq!(content, ENGRAM_MD_CONTENT);
    }

    #[test]
    fn test_write_engram_md_idempotent() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        write_engram_md(tmp.path(), false, &f).unwrap();
        let changed = write_engram_md(tmp.path(), false, &f).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_write_engram_md_updates_stale() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        std::fs::write(tmp.path().join("ENGRAM.md"), "old content").unwrap();
        let changed = write_engram_md(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("ENGRAM.md")).unwrap();
        assert_eq!(content, ENGRAM_MD_CONTENT);
    }

    #[test]
    fn test_write_engram_md_dry_run_no_write() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let changed = write_engram_md(tmp.path(), true, &f).unwrap();
        assert!(changed);
        assert!(!tmp.path().join("ENGRAM.md").exists());
    }

    // --- CLAUDE.md tests ---

    #[test]
    fn test_update_claude_md_creates_file() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let changed = update_claude_md(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "@ENGRAM.md\n");
    }

    #[test]
    fn test_update_claude_md_appends_to_existing() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();
        std::fs::write(
            tmp.path().join("CLAUDE.md"),
            "# My Project\n\nSome rules.\n",
        )
        .unwrap();

        let changed = update_claude_md(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(content.starts_with("# My Project"));
        assert!(content.ends_with("@ENGRAM.md\n"));
        assert!(content.contains("\n\n@ENGRAM.md\n"));
    }

    #[test]
    fn test_update_claude_md_no_trailing_newline() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();
        std::fs::write(tmp.path().join("CLAUDE.md"), "no trailing newline").unwrap();

        update_claude_md(tmp.path(), false, &f).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains("\n\n@ENGRAM.md\n"));
    }

    #[test]
    fn test_update_claude_md_idempotent() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        update_claude_md(tmp.path(), false, &f).unwrap();
        let changed = update_claude_md(tmp.path(), false, &f).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_update_claude_md_detects_existing_ref() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();
        std::fs::write(
            tmp.path().join("CLAUDE.md"),
            "# Project\n\n@ENGRAM.md\n\nOther stuff.\n",
        )
        .unwrap();

        let changed = update_claude_md(tmp.path(), false, &f).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_update_claude_md_dry_run_no_write() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let changed = update_claude_md(tmp.path(), true, &f).unwrap();
        assert!(changed);
        assert!(!tmp.path().join("CLAUDE.md").exists());
    }

    // --- Settings fallback tests (hooks + MCP) ---

    #[test]
    fn test_settings_fallback_creates_settings() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let changed = install_settings_fallback(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        // Hooks
        assert!(settings["hooks"]["PreToolUse"].is_array());
        assert!(settings["hooks"]["SessionStart"].is_array());
        let pre_tool_cmd = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(pre_tool_cmd.contains("engramdb hook pre-tool-use"));
        let session_cmd = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(session_cmd.contains("engramdb hook session-start"));

        // MCP server
        let mcp = &settings["mcpServers"]["engramdb"];
        assert_eq!(mcp["command"].as_str().unwrap(), "engramdb");
        assert_eq!(mcp["args"][0].as_str().unwrap(), "serve");
    }

    #[test]
    fn test_settings_fallback_idempotent() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        install_settings_fallback(tmp.path(), false, &f).unwrap();
        let changed = install_settings_fallback(tmp.path(), false, &f).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_settings_fallback_merges_with_existing() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let existing = json!({
            "permissions": { "allow": ["Read"] },
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{ "type": "command", "command": "some-other-hook" }]
                }]
            }
        });
        std::fs::write(
            tmp.path().join("settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let changed = install_settings_fallback(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        // Existing permission preserved
        assert!(settings["permissions"]["allow"].is_array());

        // Existing hook preserved + new one added
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 2);

        // SessionStart added
        assert!(settings["hooks"]["SessionStart"].is_array());

        // MCP server added
        assert!(settings["mcpServers"]["engramdb"].is_object());
    }

    #[test]
    fn test_settings_fallback_dry_run_no_write() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        let changed = install_settings_fallback(tmp.path(), true, &f).unwrap();
        assert!(changed);
        assert!(!tmp.path().join("settings.json").exists());
    }

    // --- MCP server tests ---

    #[test]
    fn test_ensure_mcp_server_adds_new() {
        let mut settings = json!({});
        let added = ensure_mcp_server(&mut settings);
        assert!(added);
        assert_eq!(
            settings["mcpServers"]["engramdb"]["command"]
                .as_str()
                .unwrap(),
            "engramdb"
        );
    }

    #[test]
    fn test_ensure_mcp_server_skips_existing() {
        let mut settings = json!({
            "mcpServers": {
                "engramdb": { "command": "engramdb", "args": ["serve"] }
            }
        });
        let added = ensure_mcp_server(&mut settings);
        assert!(!added);
    }

    // --- ensure_hook_entry tests ---

    #[test]
    fn test_ensure_hook_entry_adds_new() {
        let mut hooks = json!({});
        let entry = json!({ "hooks": [{ "type": "command", "command": "my-hook" }] });

        let added = ensure_hook_entry(&mut hooks, "PreToolUse", entry, "my-hook");
        assert!(added);
        assert_eq!(hooks["PreToolUse"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_ensure_hook_entry_skips_duplicate() {
        let mut hooks = json!({
            "PreToolUse": [{
                "hooks": [{ "type": "command", "command": "my-hook --dir ." }]
            }]
        });
        let entry = json!({ "hooks": [{ "type": "command", "command": "my-hook --dir ." }] });

        let added = ensure_hook_entry(&mut hooks, "PreToolUse", entry, "my-hook");
        assert!(!added);
        assert_eq!(hooks["PreToolUse"].as_array().unwrap().len(), 1);
    }

    // --- Path resolution tests ---

    #[test]
    fn test_resolve_claude_dir_default() {
        let project = Path::new("/tmp/my-project");
        let dir = resolve_claude_dir(project, false, None);
        assert_eq!(dir, PathBuf::from("/tmp/my-project/.claude"));
    }

    #[test]
    fn test_resolve_claude_dir_global() {
        let project = Path::new("/tmp/my-project");
        let dir = resolve_claude_dir(project, true, None);
        let home = dirs::home_dir().unwrap();
        assert_eq!(dir, home.join(".claude"));
    }

    #[test]
    fn test_resolve_claude_dir_override() {
        let project = Path::new("/tmp/my-project");
        let override_dir = Path::new("/tmp/custom-claude");
        let dir = resolve_claude_dir(project, false, Some(override_dir));
        assert_eq!(dir, PathBuf::from("/tmp/custom-claude"));
    }

    // --- Full run_setup tests ---

    #[tokio::test]
    async fn test_run_setup_no_plugin_creates_all_files() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            true,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        assert!(claude_dir.join("ENGRAM.md").exists());
        assert!(claude_dir.join("CLAUDE.md").exists());
        assert!(claude_dir.join("settings.json").exists());
    }

    #[tokio::test]
    async fn test_run_setup_idempotent() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            true,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        let engram = std::fs::read_to_string(claude_dir.join("ENGRAM.md")).unwrap();
        let claude = std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap();
        let settings = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();

        run_setup_inner(
            tmp.path(),
            true,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(claude_dir.join("ENGRAM.md")).unwrap(),
            engram
        );
        assert_eq!(
            std::fs::read_to_string(claude_dir.join("CLAUDE.md")).unwrap(),
            claude
        );
        assert_eq!(
            std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(),
            settings
        );
    }

    #[tokio::test]
    async fn test_run_setup_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            true,
            false,
            true,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        assert!(!tmp.path().join("ENGRAM.md").exists());
        assert!(!claude_dir.join("CLAUDE.md").exists());
        assert!(!claude_dir.join("settings.json").exists());
    }

    #[tokio::test]
    async fn test_run_setup_global_uses_override_dir() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("global-claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            true,
            true,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        assert!(claude_dir.join("ENGRAM.md").exists());
        assert!(claude_dir.join("CLAUDE.md").exists());
        assert!(claude_dir.join("settings.json").exists());
        assert!(!tmp.path().join("ENGRAM.md").exists());
    }

    #[tokio::test]
    async fn test_run_setup_settings_contain_mcp() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            true,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        let mcp = &settings["mcpServers"]["engramdb"];
        assert_eq!(mcp["command"].as_str().unwrap(), "engramdb");
        assert_eq!(mcp["args"][0].as_str().unwrap(), "serve");
        assert_eq!(mcp["args"][1].as_str().unwrap(), "--dir");
        assert_eq!(mcp["args"][2].as_str().unwrap(), ".");
    }

    #[tokio::test]
    async fn test_run_setup_default_project_layout() {
        let tmp = TempDir::new().unwrap();
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(tmp.path(), true, false, false, None, Some(&plugins_dir), &f)
            .await
            .unwrap();

        assert!(tmp.path().join(".claude/ENGRAM.md").exists());
        assert!(tmp.path().join(".claude/CLAUDE.md").exists());
        assert!(tmp.path().join(".claude/settings.json").exists());
    }

    #[tokio::test]
    async fn test_run_setup_global_plugin_fallback_when_claude_missing() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        // global mode, no_plugin=false, but claude CLI won't be available in test env,
        // so it should fall back to settings.json
        run_setup_inner(
            tmp.path(),
            false,
            true,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        assert!(claude_dir.join("settings.json").exists());
        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();
        assert!(settings["hooks"]["PreToolUse"].is_array());
        assert!(settings["mcpServers"]["engramdb"].is_object());
    }

    #[tokio::test]
    async fn test_run_setup_skips_hooks_mcp_when_plugin_installed() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir_with_engram(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            false,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        assert!(claude_dir.join("ENGRAM.md").exists());
        assert!(claude_dir.join("CLAUDE.md").exists());

        // settings.json IS created for permissions, but should NOT have hooks or MCP
        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();
        assert!(settings.get("hooks").is_none());
        assert!(settings.get("mcpServers").is_none());

        // Should have plugin-style permissions
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        let perms: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
        assert!(perms.iter().any(|p| p.starts_with(PLUGIN_MCP_PREFIX)));
        assert!(!perms.iter().any(|p| p.starts_with(SETTINGS_MCP_PREFIX)));
    }

    #[tokio::test]
    async fn test_run_setup_fallback_uses_settings_mcp_permissions() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            true,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        // Should have settings-style MCP permissions
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        let perms: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
        assert!(perms.iter().any(|p| p.starts_with(SETTINGS_MCP_PREFIX)));
        assert!(!perms.iter().any(|p| p.starts_with(PLUGIN_MCP_PREFIX)));
    }

    #[tokio::test]
    async fn test_permissions_cover_all_tools() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir_with_engram(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            false,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();
        let allow = settings["permissions"]["allow"].as_array().unwrap();
        let perms: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();

        for suffix in MCP_TOOL_SUFFIXES {
            let expected = format!("{PLUGIN_MCP_PREFIX}{suffix}");
            assert!(
                perms.contains(&expected.as_str()),
                "Missing permission: {expected}"
            );
        }
    }

    #[tokio::test]
    async fn test_permissions_idempotent() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir_with_engram(&tmp);
        let f = test_formatter();

        run_setup_inner(
            tmp.path(),
            false,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        let first = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();

        run_setup_inner(
            tmp.path(),
            false,
            false,
            false,
            Some(&claude_dir),
            Some(&plugins_dir),
            &f,
        )
        .await
        .unwrap();

        let second = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        assert_eq!(first, second);
    }
}
