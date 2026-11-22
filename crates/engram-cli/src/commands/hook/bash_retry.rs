//! The Bash retry prompt: command keys, masked pipeline failures, and the
//! PostToolUse/PostToolUseFailure half that records and prompts.

use super::*;

const RETRY_NOTE_PREFIX: &str = "[EngramDB]";
const RETRY_NOTE: &str = "failed earlier in this session and has now succeeded. If finding the \
cause took more than one try and nothing in the repo or the error message states it, save it now \
with `create` (type hazard: the command, the symptom, the cause and the fix). Save it yourself \
rather than offering to, unless the user asked you not to save memories. Skip this if the docs or \
the error message already said what to do, or if the fix was a change to the code under test.";

/// Programs whose runs are looking around, not the task: their failures are
/// probes (`ls var`, `cat missing.txt`) and never prompt.
const PROBE_PROGRAMS: &[&str] = &[
    "ls", "cat", "echo", "printf", "grep", "rg", "cd", "head", "tail", "sed", "awk", "find",
    "which", "pwd", "test", "[", "true", "false", "rm", "mkdir", "cp", "mv", "touch", "wc", "sort",
    "uniq", "export", "git", "diff", "file", "stat", "tree", "less", "more", "xargs", "tee",
    "date", "sleep", "chmod", "du", "df", "ps", "kill", "sqlite3", "jq", "set", "source", ".",
    "type",
];
/// Wrappers that run the next word as the real program.
const WRAPPERS: &[&str] = &[
    "env", "sudo", "nice", "time", "command", "exec", "nohup", "npx", "bunx",
];
/// Interpreters: the script or `-m` module they run is the command's identity.
const INTERPRETERS: &[&str] = &[
    "python", "python3", "node", "bash", "sh", "zsh", "ruby", "perl", "deno",
];
/// Tools whose first argument names the action (`make test`, `cargo build`).
const SUBCOMMAND_TOOLS: &[&str] = &[
    "make", "cargo", "npm", "yarn", "pnpm", "go", "docker", "just", "uv", "poetry", "pipenv",
    "bundle", "gradle", "mvn", "dotnet",
];
/// Output that means a piped command failed even though the pipeline's exit
/// status (the last stage's, e.g. `tail`) was 0.
const MASKED_FAILURE_SIGNS: &[&str] = &[
    "Traceback (most recent call last)",
    "] Error ",
    "FAILED",
    "command not found",
    "No such file or directory",
    "Permission denied",
    "panicked at",
];

/// Stable identities of the commands a shell line runs, ignoring `cd`,
/// probes, `VAR=value` prefixes, wrappers, flags and redirections. Two runs of
/// the same script with different flags or environment share a key, which is
/// what lets a failure and its later fix be matched.
/// Subcommands that run something else named by the next word (`npm run
/// lint`, `uv run pytest`, `bundle exec rspec`): the key keeps that word, or
/// every script of the tool would share one key.
const RUN_SUBCOMMANDS: &[&str] = &["run", "exec", "compose", "x", "dlx"];

/// The command line with quoted text emptied and heredoc bodies removed, so
/// neither a commit message nor an inline script is read as commands.
fn strip_quotes_and_heredocs(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut chars = command.chars().peekable();
    let mut heredoc_ends: Vec<String> = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '\'' | '"' => {
                // Keep an empty pair so an argument position stays an argument.
                out.push(c);
                let mut escaped = false;
                for q in chars.by_ref() {
                    if c == '"' && q == '\\' && !escaped {
                        escaped = true;
                        continue;
                    }
                    if q == c && !escaped {
                        break;
                    }
                    escaped = false;
                }
                out.push(c);
            }
            '<' if chars.peek() == Some(&'<') => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    chars.next(); // here-string `<<<`: the word is data
                    continue;
                }
                if chars.peek() == Some(&'-') {
                    chars.next();
                }
                while chars.peek().is_some_and(|c| *c == ' ') {
                    chars.next();
                }
                let word: String = std::iter::from_fn(|| {
                    chars.next_if(|c| c.is_alphanumeric() || matches!(c, '_' | '\'' | '"'))
                })
                .filter(|c| *c != '\'' && *c != '"')
                .collect();
                if !word.is_empty() {
                    heredoc_ends.push(word);
                }
            }
            '\n' if !heredoc_ends.is_empty() => {
                out.push('\n');
                // Drop each pending heredoc body, up to and including its end line.
                for end in std::mem::take(&mut heredoc_ends) {
                    loop {
                        let line: String =
                            std::iter::from_fn(|| chars.next_if(|c| *c != '\n')).collect();
                        let at_end = chars.next().is_none();
                        if line.trim() == end || at_end {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A key that names a program or script, not a stray word.
pub(super) fn plausible_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('-')
        && key.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+' | '@' | ' ')
        })
}

pub(super) fn command_keys(command: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let stripped = strip_quotes_and_heredocs(command);
    let segments = stripped.split(['\n', ';', '|', '&']);
    for segment in segments {
        let mut words = segment
            .split_whitespace()
            .map(|w| w.trim_matches(|c| c == '"' || c == '\'' || c == '(' || c == ')'))
            .filter(|w| !w.is_empty())
            .peekable();
        // Leading `VAR=value` assignments and wrappers (with `timeout N`).
        let program = loop {
            let Some(w) = words.next() else { break None };
            let is_assignment = w.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            if is_assignment || WRAPPERS.contains(&w) {
                continue;
            }
            if w == "timeout" {
                if words
                    .peek()
                    .is_some_and(|n| n.chars().next().is_some_and(|c| c.is_ascii_digit()))
                {
                    words.next();
                }
                continue;
            }
            break Some(w);
        };
        let Some(program) = program else { continue };
        let base = program.rsplit('/').next().unwrap_or(program);
        // python3.11 -> python3
        let base = base
            .split_once('.')
            .filter(|(head, tail)| {
                head.starts_with("python") && tail.chars().all(|c| c.is_ascii_digit())
            })
            .map_or(base, |(head, _)| head);
        if PROBE_PROGRAMS.contains(&base)
            || base.starts_with(|c: char| c.is_ascii_digit() || c == '>')
        {
            continue;
        }
        let mut args = words.filter(|w| {
            !w.contains(['>', '<']) && (!w.starts_with('-') || matches!(*w, "-m" | "-c" | "-e"))
        });
        let key = if INTERPRETERS.contains(&base) {
            match args.next() {
                Some("-m") => args.next().map(str::to_string),
                Some("-c" | "-e") | None => None,
                Some(script) => Some(script.trim_start_matches("./").to_string()),
            }
        } else if SUBCOMMAND_TOOLS.contains(&base) {
            Some(match args.next() {
                Some(sub) if RUN_SUBCOMMANDS.contains(&sub) => match args.next() {
                    Some(target) => format!("{base} {sub} {target}"),
                    None => format!("{base} {sub}"),
                },
                Some(sub) => format!("{base} {sub}"),
                None => base.to_string(),
            })
        } else {
            Some(base.to_string())
        };
        if let Some(key) = key.filter(|k| plausible_key(k)) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

/// Whether a Bash result that Claude Code reported as a success actually
/// failed: only for a pipeline (whose exit status is the last stage's) with
/// unmistakable failure output.
fn masked_failure(command: &str, value: &serde_json::Value) -> bool {
    if !command.contains('|') {
        return false;
    }
    let response = value.get("tool_response");
    let text = |field: &str| {
        response
            .and_then(|r| r.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    let output = format!("{}\n{}", text("stdout"), text("stderr"));
    MASKED_FAILURE_SIGNS
        .iter()
        .any(|sign| output.contains(sign))
}

/// The Bash half of the PostToolUse hook (and all of PostToolUseFailure):
/// record a failed command, and when a later run of the same command
/// succeeds, prompt once to save what it took. Returns the hook response, if
/// any. Every error is swallowed: this is advice, never a blocker.
pub(super) fn process_bash_retry(value: &serde_json::Value, dir: &Path) -> Option<String> {
    // This writes state, so it bails for a directory that was never `init`ed,
    // like SessionEnd (the plugin registers hooks machine-wide).
    let project_dir = engramdb::storage::paths::project_dir(dir);
    if !project_dir.join("manifest.toml").exists() && !project_dir.join("memories").is_dir() {
        return None;
    }
    let session_id = &extract_context_id(&value.to_string())?;
    let command = value.get("tool_input")?.get("command")?.as_str()?;
    let keys = command_keys(command);
    if keys.is_empty() {
        return None;
    }
    let failed = value.get("hook_event_name").and_then(|v| v.as_str())
        == Some("PostToolUseFailure")
        || masked_failure(command, value);
    if failed {
        if let Err(e) = engramdb::storage::bash_retry::record_failure(dir, session_id, &keys) {
            tracing::debug!("retry record write failed (non-fatal): {e}");
        }
        return None;
    }
    let pending = engramdb::storage::bash_retry::pending_failures(dir, session_id);
    let fixed: Vec<String> = keys.into_iter().filter(|k| pending.contains(k)).collect();
    if fixed.is_empty() {
        return None;
    }
    if let Err(e) = engramdb::storage::bash_retry::mark_prompted(dir, session_id, &fixed) {
        // Without the record the prompt would repeat on every later success.
        tracing::debug!("retry record write failed, not prompting: {e}");
        return None;
    }
    let names = fixed
        .iter()
        .map(|k| format!("`{}`", engramdb::storage::transcripts::sanitize_one_line(k)))
        .collect::<Vec<_>>()
        .join(", ");
    build_hook_response(
        "PostToolUse",
        &format!("{RETRY_NOTE_PREFIX} {names} {RETRY_NOTE}"),
    )
    .ok()
}
