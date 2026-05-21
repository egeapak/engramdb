# Claude Code Integration

EngramDB plugs into Claude Code in three ways, all of which run automatically once installed:

1. **MCP server** — Claude Code talks to `engramdb serve` and gets a suite of memory tools.
2. **SessionStart hook** — at the start of every session, high-criticality memories are injected as `additionalContext`.
3. **PreToolUse hook** — when Claude reads, writes, or edits a file, relevant memories are surfaced as `additionalContext`.

There are two ways to wire this up: the plugin (recommended) or `engramdb setup`.

## Option A: the plugin (recommended)

The `engram` plugin lives in the same GitHub repo and bundles the hooks, MCP server, and permissions.

```bash
# inside a Claude Code session
/plugin marketplace add egeapak/engramdb
/plugin install engram@engramdb
```

After install, restart the session. To update:

```bash
/plugin update engram@engramdb
```

To register MCP-tool permissions in your `settings.json` (otherwise Claude will prompt for each tool the first time it's called), run once:

```bash
engramdb setup --global
```

When the plugin is detected, `setup --global` writes the correct `mcp__plugin_engram_memory__*` permission entries instead of duplicating hooks.

### What the plugin includes

- `mcpServers.memory` — runs `engramdb serve --dir .`.
- `hooks.PreToolUse` — matcher `Read|Write|Edit`, runs `engramdb hook pre-tool-use --dir .`.
- `hooks.SessionStart` — runs `engramdb hook session-start --dir .`.

The plugin manifest is at `.claude-plugin/plugin.json` if you want to inspect it.

## Option B: `engramdb setup` (no plugin)

`engramdb setup` writes the same hooks and MCP entry directly into `settings.json`, without any plugin machinery.

```bash
# Project-scoped: writes to <project>/.claude/settings.json
engramdb setup

# Global: writes to ~/.claude/settings.json
engramdb setup --global

# Show the diff without applying:
engramdb setup --global --dry-run

# Skip the plugin path entirely in global mode and write hooks directly:
engramdb setup --global --no-plugin
```

Both modes also:
- create or update `ENGRAM.md` in the target directory (it's the directive file the agent reads),
- add `@ENGRAM.md` to the relevant `CLAUDE.md` so Claude Code loads it,
- write project-local `.engramdb/` if missing.

## What gets written

Snapshot of the relevant `settings.json` shape after `setup --global`:

```jsonc
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read|Write|Edit",
        "hooks": [{ "type": "command", "command": "engramdb hook pre-tool-use --dir ." }]
      }
    ],
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "engramdb hook session-start --dir ." }] }
    ]
  },
  "mcpServers": {
    "engramdb": { "command": "engramdb", "args": ["serve", "--dir", "."] }
  }
}
```

If you already had an `engramdb` `mcpServers` entry, `setup` updates it in place. If you've **also** installed the plugin, `setup` detects that and skips writing the hooks/mcpServers to avoid duplicates — it only manages permissions.

## How the hooks behave

### `SessionStart`

Reads the event JSON from stdin and emits `additionalContext` listing high-criticality memories (criticality ≥ `--min-criticality`, default `0.6`). The output is capped at ~2000 characters so it doesn't blow up the prompt. Only the most relevant memories survive the budget — see `SESSION_CONTEXT_BUDGET` in `src/cli/commands/hook.rs`.

A typical session-start injection looks like:

```
[hazard] LanceDB advisory lock is per-project; concurrent writes from different processes serialize (criticality: 0.9)
[convention] Memories always use TOML frontmatter; never YAML (criticality: 0.8)
```

### `PreToolUse (Read|Write|Edit)`

Reads the event JSON from stdin, extracts `tool_input.file_path`, and runs a `rank`-mode query with that path as the context. Results are formatted as a compact list and emitted as `additionalContext`. The agent sees this just before the tool call runs.

Example output for a `Read` of `src/db/connection.rs`:

```
[hazard] PostgreSQL connection pool is shared globally — don't create new pools (criticality: 0.85, score: 0.78)
[decision] Use PgBouncer in production for transaction-level pooling (criticality: 0.8, score: 0.71)
```

If the file path can't be relativized to the project root, the absolute path is used.

## Troubleshooting

**Plugin not loading.** Make sure `engramdb` is on Claude Code's `PATH`. Run `engramdb doctor` — if it doesn't run, fix that first.

**"Tool requires approval" prompts every time.** Permissions aren't set. Run `engramdb setup --global` once to register the `mcp__*` permission entries in `~/.claude/settings.json`.

**Duplicate hook output.** You installed the plugin **and** ran `engramdb setup` (which wrote hooks to `settings.json`). Remove the `engramdb` entries under `hooks` and `mcpServers` in `~/.claude/settings.json` — the plugin manages them now.

**No memories surfaced even though the store has data.** The hook needs the working directory passed via `--dir`. The plugin uses `--dir .` so Claude Code's CWD matters. If you've changed working directory mid-session, the hook may be querying a different project. Run `engramdb projects info` in the session to confirm which project is being targeted.

**Stale permissions after update.** Run `engramdb setup --global` again — it refreshes the entries.

**MCP server fails to start.** Run `engramdb serve --dir <project>` manually to see the error. The most common cause is a missing protoc or a corrupted model cache. `engramdb doctor` will pinpoint it.

## Disabling

To disable engramdb for a session without uninstalling:

```bash
# Plugin
/plugin disable engram@engramdb

# Manual setup
# Edit ~/.claude/settings.json and remove the engramdb hooks + mcpServers entries
```

Per-project disable: delete `<project>/.engramdb/` and the project's hooks won't trigger for that directory.
