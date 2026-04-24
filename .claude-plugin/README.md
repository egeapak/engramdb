# EngramDB Plugin for Claude Code

Persistent memory for your coding agent. This plugin gives Claude Code the ability to remember decisions, hazards, conventions, and context about your codebase across sessions.

## Prerequisites

EngramDB must be installed and available on your PATH:

```bash
cargo install --git https://github.com/egeapak/engramdb
```

Verify with:

```bash
engramdb --version
```

## Installation

### From the marketplace

```bash
# Add the marketplace (once)
/plugin marketplace add egeapak/engramdb

# Install
/plugin install engram@engramdb
```

### Update

```bash
/plugin update engram@engramdb
```

## What You Get

### MCP Server

A full MCP server (`engramdb serve`) starts automatically, providing 15 tools for memory management:

`query`, `create`, `get`, `list`, `update`, `delete`, `challenge`, `review`, `resolve`, `stats`, `doctor`, `gc`, `reindex`, `compress_candidates`, `compress_apply`

### Hooks

- **SessionStart** — injects high-criticality memories (hazards, active decisions) into the conversation when a session begins
- **PreToolUse (Read/Write/Edit)** — surfaces relevant memories as context when the agent touches files

### Permissions

Run `engramdb setup --global` to auto-configure MCP tool permissions in your `settings.json`. If the plugin is detected, it writes the correct `mcp__plugin_engram_memory__*` permission entries.

## Usage

Once installed, the plugin works automatically. Your agent will:

- **Query in filter mode** before answering project questions about conventions, architecture, or workflows
- **Query in rank mode** before modifying files, surfacing memories relevant to the current path or scope
- **Store after discovering** important patterns, decisions, or conventions
- **Challenge contradictions** when it finds information that conflicts with existing memories

### Storing memories manually

You can also use the CLI directly:

```bash
engramdb add --type hazard --title "Vector store needs reindex after schema changes" \
  "Changing memory schema requires running engramdb reindex to rebuild embeddings."

engramdb query --mode filter "schema migration"
```

### Agent directives

For best results, add `@ENGRAM.md` to your project's `CLAUDE.md`. The setup command does this automatically:

```bash
engramdb setup        # project-scoped
engramdb setup --global  # global
```

## Troubleshooting

**Plugin not loading:** Ensure `engramdb` is on your PATH. Run `engramdb doctor` to check.

**Duplicate hooks:** If you previously used `engramdb setup` to write hooks to `settings.json` and then installed the plugin, remove the `hooks` and `mcpServers` engramdb entries from `~/.claude/settings.json` to avoid duplicates.

**Stale permissions:** After updating the plugin, run `engramdb setup --global` to refresh permission entries.
