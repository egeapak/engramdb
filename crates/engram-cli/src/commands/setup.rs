//! Set up Claude Code integration for the current project.

use crate::output::OutputFormatter;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGRAM_MD_CONTENT: &str = r#"# EngramDB

This project uses EngramDB for persistent agent memory.

- **Expand surfaced memories** — when memories are surfaced at session start, `get` the full content of any relevant to the current task before proceeding.
- **Query before answering or modifying** — call `query` with `mode: "rank"` to surface memories relevant to your current file, topic, or logical scope; call with `mode: "filter"` (plus a `query` text, `logical` scopes, `path`, or `tags`) when you need specific-term lookup.
- **Store after discovering** — call `create` after discovering important patterns, decisions, hazards, or conventions worth preserving.
- **Challenge contradictions** — call `challenge` when you find information that contradicts an existing memory.
"#;

const ENGRAM_MD_REF: &str = "@ENGRAM.md";

/// MCP tool suffixes that need permission entries.
///
/// Must cover **every** tool the MCP server exposes — a missing entry means a
/// permission prompt on every invocation of that tool. Pinned against the
/// server's actual tool router by `mcp_tool_suffixes_match_server_tools`.
const MCP_TOOL_SUFFIXES: &[&str] = &[
    "query",
    "create",
    "get",
    "list",
    "update",
    "delete",
    "challenge",
    "resolve",
    "verify",
    "review",
    "stats",
    "doctor",
    "gc",
    "reindex",
    "compress_candidates",
    "compress_apply",
    "projects_list",
    "projects_info",
    "projects_link",
    "projects_unlink",
];

/// MCP tool suffixes that were removed or renamed and must be stripped from
/// existing settings.json permission lists during setup. Without this cleanup,
/// stale allowlist entries under the old names silently stop matching the
/// current tools and the user gets a permission prompt every invocation.
const STALE_MCP_TOOL_SUFFIXES: &[&str] = &["search", "retrieve"];

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

/// Human-readable JSON type name for error messages.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Error for a settings.json key that has the wrong JSON type. Names the file
/// and the offending key so the user can fix it by hand.
fn shape_error(
    settings_path: &Path,
    key_path: &str,
    expected: &str,
    found: &Value,
) -> anyhow::Error {
    anyhow!(
        "{}: expected \"{key_path}\" to be a JSON {expected}, found {} — fix or remove the key and re-run engramdb setup",
        settings_path.display(),
        json_type_name(found)
    )
}

/// True if a value is semantically empty (null, `[]`, or `{}`) and therefore
/// safe to replace with a freshly shaped container without losing user data.
fn is_semantically_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(arr) => arr.is_empty(),
        Value::Object(obj) => obj.is_empty(),
        _ => false,
    }
}

/// Read and parse settings.json, validating that the top level is a JSON
/// object we can merge into. A missing file yields an empty object.
fn read_settings(settings_path: &Path) -> Result<Value> {
    let settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(settings_path)
            .with_context(|| format!("failed to read {}", settings_path.display()))?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "{}: not valid JSON — fix the file and re-run engramdb setup",
                settings_path.display()
            )
        })?
    } else {
        json!({})
    };

    if !settings.is_object() {
        return Err(anyhow!(
            "{}: expected the top-level value to be a JSON object, found {} — fix the file and re-run engramdb setup",
            settings_path.display(),
            json_type_name(&settings)
        ));
    }
    Ok(settings)
}

/// Borrow the top-level settings object, erroring (not panicking) if the
/// value is not a JSON object.
fn settings_object_mut<'a>(
    settings: &'a mut Value,
    settings_path: &Path,
) -> Result<&'a mut serde_json::Map<String, Value>> {
    match settings {
        Value::Object(map) => Ok(map),
        other => Err(anyhow!(
            "{}: expected the top-level value to be a JSON object, found {} — fix the file and re-run engramdb setup",
            settings_path.display(),
            json_type_name(other)
        )),
    }
}

/// Get-or-create `parent[key]` as a JSON object. A missing key is inserted as
/// `{}`; a semantically empty value (null, `[]`, `{}`) is repaired to `{}`;
/// any other non-object type is a hard error naming the key.
fn ensure_object_entry<'a>(
    parent: &'a mut serde_json::Map<String, Value>,
    key: &str,
    key_path: &str,
    settings_path: &Path,
) -> Result<&'a mut serde_json::Map<String, Value>> {
    let slot = parent.entry(key).or_insert_with(|| json!({}));
    if !slot.is_object() && is_semantically_empty(slot) {
        *slot = json!({});
    }
    match slot {
        Value::Object(map) => Ok(map),
        other => Err(shape_error(settings_path, key_path, "object", other)),
    }
}

/// Get-or-create `parent[key]` as a JSON array. A missing key is inserted as
/// `[]`; a semantically empty value (null, `[]`, `{}`) is repaired to `[]`;
/// any other non-array type is a hard error naming the key.
fn ensure_array_entry<'a>(
    parent: &'a mut serde_json::Map<String, Value>,
    key: &str,
    key_path: &str,
    settings_path: &Path,
) -> Result<&'a mut Vec<Value>> {
    let slot = parent.entry(key).or_insert_with(|| json!([]));
    if !slot.is_array() && is_semantically_empty(slot) {
        *slot = json!([]);
    }
    match slot {
        Value::Array(arr) => Ok(arr),
        other => Err(shape_error(settings_path, key_path, "array", other)),
    }
}

/// Atomically replace `path` with `contents`: write a temp file in the same
/// directory, then rename it over the target. A crash mid-write can therefore
/// never leave a truncated or corrupt settings.json behind.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("settings.json");
    let tmp_path = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));

    let result = std::fs::write(&tmp_path, contents)
        .and_then(|()| std::fs::rename(&tmp_path, path))
        .with_context(|| format!("failed to write {}", path.display()));
    if result.is_err() {
        // Best-effort cleanup; never leave a temp file lying around.
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

/// Write hooks and MCP server config into settings.json (merge strategy).
fn install_settings_fallback(
    claude_dir: &Path,
    dry_run: bool,
    formatter: &OutputFormatter,
) -> Result<bool> {
    let settings_path = claude_dir.join("settings.json");

    let mut settings = read_settings(&settings_path)?;

    let mut changed = false;

    // --- Hooks ---
    let root = settings_object_mut(&mut settings, &settings_path)?;
    let hooks = ensure_object_entry(root, "hooks", "hooks", &settings_path)?;

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
        &settings_path,
    )?;

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
        &settings_path,
    )?;

    changed |= ensure_hook_entry(
        hooks,
        "UserPromptSubmit",
        json!({
            "hooks": [{
                "type": "command",
                "command": "engramdb hook user-prompt-submit --dir ."
            }]
        }),
        "engramdb hook user-prompt-submit",
        &settings_path,
    )?;

    changed |= ensure_hook_entry(
        hooks,
        "PostToolUse",
        json!({
            "matcher": "Write|Edit|MultiEdit",
            "hooks": [{
                "type": "command",
                "command": "engramdb hook post-tool-use --dir ."
            }]
        }),
        "engramdb hook post-tool-use",
        &settings_path,
    )?;

    changed |= ensure_hook_entry(
        hooks,
        "SessionEnd",
        json!({
            "hooks": [{
                "type": "command",
                "command": "engramdb hook session-end --dir ."
            }]
        }),
        "engramdb hook session-end",
        &settings_path,
    )?;

    changed |= ensure_hook_entry(
        hooks,
        "PreCompact",
        json!({
            "hooks": [{
                "type": "command",
                "command": "engramdb hook pre-compact --dir ."
            }]
        }),
        "engramdb hook pre-compact",
        &settings_path,
    )?;

    // --- MCP server ---
    changed |= ensure_mcp_server(&mut settings, &settings_path)?;

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
    write_atomic(&settings_path, &formatted)?;
    formatter.print_success("Added hooks and MCP server to settings.json.");
    Ok(true)
}

/// Ensure the engramdb MCP server entry exists in settings.json.
/// Returns true if it was added.
fn ensure_mcp_server(settings: &mut Value, settings_path: &Path) -> Result<bool> {
    let root = settings_object_mut(settings, settings_path)?;
    let mcp_servers = ensure_object_entry(root, "mcpServers", "mcpServers", settings_path)?;

    if mcp_servers.get("engramdb").is_some() {
        return Ok(false);
    }

    mcp_servers.insert(
        "engramdb".to_string(),
        json!({
            "command": "engramdb",
            "args": ["serve", "--dir", "."]
        }),
    );
    Ok(true)
}

/// Ensure a hook entry exists in the given event array, matched by command substring.
/// Foreign elements of unexpected types (non-objects, objects without a
/// `hooks` array) are left untouched and skipped during matching.
/// Returns true if a new entry was added.
fn ensure_hook_entry(
    hooks: &mut serde_json::Map<String, Value>,
    event: &str,
    entry: Value,
    match_command: &str,
    settings_path: &Path,
) -> Result<bool> {
    let event_array = ensure_array_entry(hooks, event, &format!("hooks.{event}"), settings_path)?;

    let already_exists = event_array.iter().any(|e| {
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
        return Ok(false);
    }

    event_array.push(entry);
    Ok(true)
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

    let mut settings = read_settings(&settings_path)?;

    let root = settings_object_mut(&mut settings, &settings_path)?;
    let permissions = ensure_object_entry(root, "permissions", "permissions", &settings_path)?;
    let allow_arr = ensure_array_entry(permissions, "allow", "permissions.allow", &settings_path)?;

    // Remove stale entries for tools that have been renamed or removed. Stale
    // entries silently fail to match the new tool names and cause a permission
    // prompt on every invocation. Strip across BOTH the plugin and settings
    // prefixes so the cleanup works regardless of which layout the user has.
    let stale_names: std::collections::HashSet<String> = STALE_MCP_TOOL_SUFFIXES
        .iter()
        .flat_map(|suffix| {
            [
                format!("{PLUGIN_MCP_PREFIX}{suffix}"),
                format!("{SETTINGS_MCP_PREFIX}{suffix}"),
            ]
        })
        .collect();
    let before_len = allow_arr.len();
    allow_arr.retain(|v| match v.as_str() {
        Some(s) => !stale_names.contains(s),
        None => true,
    });
    let removed = before_len - allow_arr.len();

    let existing: std::collections::HashSet<String> = allow_arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let mut added = 0usize;
    for suffix in MCP_TOOL_SUFFIXES {
        let perm = format!("{prefix}{suffix}");
        if !existing.contains(&perm) {
            allow_arr.push(json!(perm));
            added += 1;
        }
    }

    if added == 0 && removed == 0 {
        formatter.print_message("MCP permissions already configured.");
        return Ok(false);
    }

    if dry_run {
        if removed > 0 {
            formatter.print_message(&format!(
                "Would remove {removed} stale MCP permission entries and add new ones."
            ));
        } else {
            formatter.print_message("Would add MCP tool permissions to settings.json.");
        }
        return Ok(true);
    }

    std::fs::create_dir_all(claude_dir)?;
    let formatted = serde_json::to_string_pretty(&settings)?;
    write_atomic(&settings_path, &formatted)?;
    if removed > 0 {
        formatter.print_success(&format!(
            "Updated MCP tool permissions in settings.json (removed {removed} stale, added {added})."
        ));
    } else {
        formatter.print_success("Added MCP tool permissions to settings.json.");
    }
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
    use crate::app::OutputFormat;
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
        let added = ensure_mcp_server(&mut settings, Path::new("settings.json")).unwrap();
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
        let added = ensure_mcp_server(&mut settings, Path::new("settings.json")).unwrap();
        assert!(!added);
    }

    // --- ensure_hook_entry tests ---

    #[test]
    fn test_ensure_hook_entry_adds_new() {
        let mut hooks = json!({});
        let entry = json!({ "hooks": [{ "type": "command", "command": "my-hook" }] });

        let added = ensure_hook_entry(
            hooks.as_object_mut().unwrap(),
            "PreToolUse",
            entry,
            "my-hook",
            Path::new("settings.json"),
        )
        .unwrap();
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

        let added = ensure_hook_entry(
            hooks.as_object_mut().unwrap(),
            "PreToolUse",
            entry,
            "my-hook",
            Path::new("settings.json"),
        )
        .unwrap();
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

    /// `MCP_TOOL_SUFFIXES` must cover exactly the tools the MCP server
    /// exposes: a missing suffix means a permission prompt on every
    /// invocation of that tool, an extra one is a stale allowlist entry.
    /// `engram-cli` depends on `engram-mcp`, so this pins directly against
    /// the server's tool router instead of a hand-maintained count.
    #[test]
    fn mcp_tool_suffixes_match_server_tools() {
        let mut server_tools = engram_mcp::EngramDbServer::tool_names();
        server_tools.sort();
        let mut suffixes: Vec<String> = MCP_TOOL_SUFFIXES.iter().map(|s| s.to_string()).collect();
        suffixes.sort();
        assert_eq!(
            suffixes, server_tools,
            "MCP_TOOL_SUFFIXES (setup.rs) drifted from the MCP server's tool surface"
        );
    }

    #[tokio::test]
    async fn test_permissions_strip_stale_search_retrieve() {
        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        let plugins_dir = fake_plugins_dir_with_engram(&tmp);
        let f = test_formatter();

        // Pre-seed settings.json with old-style search/retrieve permissions that
        // would have been written by a previous engramdb release.
        std::fs::create_dir_all(&claude_dir).unwrap();
        let preexisting = serde_json::json!({
            "permissions": {
                "allow": [
                    format!("{PLUGIN_MCP_PREFIX}search"),
                    format!("{PLUGIN_MCP_PREFIX}retrieve"),
                    format!("{SETTINGS_MCP_PREFIX}search"),
                    "Bash(ls:*)",
                ]
            }
        });
        std::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&preexisting).unwrap(),
        )
        .unwrap();

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

        for stale in STALE_MCP_TOOL_SUFFIXES {
            let plugin_name = format!("{PLUGIN_MCP_PREFIX}{stale}");
            let settings_name = format!("{SETTINGS_MCP_PREFIX}{stale}");
            assert!(
                !perms.contains(&plugin_name.as_str()),
                "Stale {plugin_name} should have been removed: {perms:?}"
            );
            assert!(
                !perms.contains(&settings_name.as_str()),
                "Stale {settings_name} should have been removed: {perms:?}"
            );
        }

        // The unrelated non-engramdb permission stays in place.
        assert!(
            perms.contains(&"Bash(ls:*)"),
            "Non-engramdb permission should be preserved: {perms:?}"
        );

        // The new query permission is present.
        let new_query = format!("{PLUGIN_MCP_PREFIX}query");
        assert!(
            perms.contains(&new_query.as_str()),
            "Expected new {new_query} permission: {perms:?}"
        );
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

    // --- Malformed settings.json shape tests ---

    fn write_settings(dir: &Path, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("settings.json"), content).unwrap();
    }

    /// Which setup entry point to drive a malformed-shape case through.
    enum Via {
        Fallback,
        Permissions,
    }

    fn run_via(via: &Via, dir: &Path, f: &OutputFormatter) -> Result<bool> {
        match via {
            Via::Fallback => install_settings_fallback(dir, false, f),
            Via::Permissions => ensure_mcp_permissions(dir, SETTINGS_MCP_PREFIX, false, f),
        }
    }

    #[test]
    fn test_malformed_settings_error_not_panic() {
        // (label, file content, entry point, expected error fragment)
        let cases: &[(&str, &str, Via, &str)] = &[
            ("top-level array", r#"["x"]"#, Via::Fallback, "top-level"),
            ("top-level string", r#""hello""#, Via::Fallback, "top-level"),
            ("top-level number", "42", Via::Permissions, "top-level"),
            (
                "hooks non-empty array",
                r#"{"hooks": ["x"]}"#,
                Via::Fallback,
                "\"hooks\"",
            ),
            (
                "hooks string",
                r#"{"hooks": "x"}"#,
                Via::Fallback,
                "\"hooks\"",
            ),
            (
                "hook event not an array",
                r#"{"hooks": {"PreToolUse": "notanarray"}}"#,
                Via::Fallback,
                "\"hooks.PreToolUse\"",
            ),
            (
                "mcpServers non-empty array",
                r#"{"mcpServers": ["x"]}"#,
                Via::Fallback,
                "\"mcpServers\"",
            ),
            (
                "permissions string",
                r#"{"permissions": "x"}"#,
                Via::Permissions,
                "\"permissions\"",
            ),
            (
                "permissions.allow non-empty object",
                r#"{"permissions": {"allow": {"a": 1}}}"#,
                Via::Permissions,
                "\"permissions.allow\"",
            ),
        ];

        let f = test_formatter();
        for (label, content, via, fragment) in cases {
            let tmp = TempDir::new().unwrap();
            write_settings(tmp.path(), content);

            let err = run_via(via, tmp.path(), &f)
                .expect_err(&format!("case {label:?} should error, not succeed"));
            let msg = format!("{err:#}");
            assert!(
                msg.contains(fragment),
                "case {label:?}: error should name {fragment}, got: {msg}"
            );
            assert!(
                msg.contains("settings.json"),
                "case {label:?}: error should name the file, got: {msg}"
            );

            // The user's file is left untouched on error.
            let after = std::fs::read_to_string(tmp.path().join("settings.json")).unwrap();
            assert_eq!(&after, content, "case {label:?}: file must not be modified");
        }
    }

    #[test]
    fn test_malformed_settings_repairs_empty_values() {
        // Semantically empty values (null, [], {}) of the wrong type are
        // repaired to the correct empty container instead of erroring.
        let cases: &[(&str, &str, Via)] = &[
            ("hooks empty array", r#"{"hooks": []}"#, Via::Fallback),
            ("hooks null", r#"{"hooks": null}"#, Via::Fallback),
            (
                "hook event empty object",
                r#"{"hooks": {"PreToolUse": {}}}"#,
                Via::Fallback,
            ),
            (
                "mcpServers empty array",
                r#"{"mcpServers": []}"#,
                Via::Fallback,
            ),
            (
                "permissions empty array",
                r#"{"permissions": []}"#,
                Via::Permissions,
            ),
            (
                "permissions.allow null",
                r#"{"permissions": {"allow": null}}"#,
                Via::Permissions,
            ),
        ];

        let f = test_formatter();
        for (label, content, via) in cases {
            let tmp = TempDir::new().unwrap();
            write_settings(tmp.path(), content);

            let changed =
                run_via(via, tmp.path(), &f).unwrap_or_else(|e| panic!("case {label:?}: {e:#}"));
            assert!(changed, "case {label:?}: repair should report a change");

            let after = std::fs::read_to_string(tmp.path().join("settings.json")).unwrap();
            let settings: Value = serde_json::from_str(&after).unwrap();
            match via {
                Via::Fallback => {
                    assert!(
                        settings["hooks"]["PreToolUse"].is_array(),
                        "case {label:?}: hooks.PreToolUse should be an array"
                    );
                    assert!(
                        settings["mcpServers"]["engramdb"].is_object(),
                        "case {label:?}: mcpServers.engramdb should be an object"
                    );
                }
                Via::Permissions => {
                    let allow = settings["permissions"]["allow"].as_array().unwrap();
                    assert!(
                        allow.iter().any(|v| {
                            v.as_str()
                                .map(|s| s.starts_with(SETTINGS_MCP_PREFIX))
                                .unwrap_or(false)
                        }),
                        "case {label:?}: permissions.allow should hold the new entries"
                    );
                }
            }
        }
    }

    #[test]
    fn test_hook_event_array_foreign_elements_preserved() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();
        write_settings(
            tmp.path(),
            r#"{"hooks": {"PreToolUse": ["junk", 42, {"weird": true}]}}"#,
        );

        let changed = install_settings_fallback(tmp.path(), false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();

        // 3 foreign elements untouched + our new entry appended.
        assert_eq!(pre_tool.len(), 4);
        assert_eq!(pre_tool[0], json!("junk"));
        assert_eq!(pre_tool[1], json!(42));
        assert_eq!(pre_tool[2], json!({"weird": true}));
        assert!(pre_tool[3]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("engramdb hook pre-tool-use"));
    }

    #[test]
    fn test_permissions_allow_foreign_elements_preserved() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();
        write_settings(
            tmp.path(),
            r#"{"permissions": {"allow": [42, {"x": 1}, "Bash(ls:*)"]}}"#,
        );

        let changed = ensure_mcp_permissions(tmp.path(), SETTINGS_MCP_PREFIX, false, &f).unwrap();
        assert!(changed);

        let content = std::fs::read_to_string(tmp.path().join("settings.json")).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();
        let allow = settings["permissions"]["allow"].as_array().unwrap();

        assert_eq!(allow[0], json!(42));
        assert_eq!(allow[1], json!({"x": 1}));
        assert_eq!(allow[2], json!("Bash(ls:*)"));
        assert_eq!(allow.len(), 3 + MCP_TOOL_SUFFIXES.len());
    }

    #[test]
    fn test_invalid_json_settings_errors_cleanly() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();
        write_settings(tmp.path(), "{not json");

        let err = install_settings_fallback(tmp.path(), false, &f).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("settings.json"), "should name the file: {msg}");
        assert!(msg.contains("not valid JSON"), "got: {msg}");

        let err = ensure_mcp_permissions(tmp.path(), SETTINGS_MCP_PREFIX, false, &f).unwrap_err();
        assert!(format!("{err:#}").contains("not valid JSON"));
    }

    // --- Atomic write tests ---

    #[test]
    fn test_atomic_write_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let f = test_formatter();

        install_settings_fallback(tmp.path(), false, &f).unwrap();
        ensure_mcp_permissions(tmp.path(), SETTINGS_MCP_PREFIX, false, &f).unwrap();

        let entries: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["settings.json".to_string()],
            "only settings.json should remain, no temp files: {entries:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_atomic_write_read_only_dir_errors_cleanly() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let claude_dir = tmp.path().join("claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Running as root ignores directory permissions — skip in that case.
        if std::fs::write(claude_dir.join(".probe"), "x").is_ok() {
            let _ = std::fs::remove_file(claude_dir.join(".probe"));
            std::fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let f = test_formatter();
        let result = install_settings_fallback(&claude_dir, false, &f);

        // Restore so TempDir cleanup succeeds.
        std::fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = result.expect_err("read-only dir should error, not panic");
        assert!(
            format!("{err:#}").contains("settings.json"),
            "error should name the file: {err:#}"
        );
        // No temp file left behind.
        assert_eq!(std::fs::read_dir(&claude_dir).unwrap().count(), 0);
    }
}
