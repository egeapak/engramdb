# Claude Code Integration

Once wired up, EngramDB exposes its MCP tool surface and runs six hooks automatically:

- **SessionStart** injects high-criticality memories as `additionalContext`, grouped by epistemic class.
- **PreToolUse (Read|Write|Edit)** surfaces memories relevant to the file being touched.
- **UserPromptSubmit** surfaces memories relevant to the prompt you just submitted.
- **PostToolUse (Write|Edit|MultiEdit)** warns when an edit touches a path a memory is watching.
- **SessionEnd** does task housekeeping (no context output).
- **PreCompact** reminds the agent to store durable discoveries before context is compacted.

Two ways to wire this up: the plugin (recommended) or `engramdb setup`.

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

The plugin manifest is at `.claude-plugin/plugin.json` — inspect it to see exactly what gets wired.

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
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "engramdb hook user-prompt-submit --dir ." }] }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [{ "type": "command", "command": "engramdb hook post-tool-use --dir ." }]
      }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "engramdb hook session-end --dir ." }] }
    ],
    "PreCompact": [
      { "hooks": [{ "type": "command", "command": "engramdb hook pre-compact --dir ." }] }
    ]
  },
  "mcpServers": {
    "engramdb": { "command": "engramdb", "args": ["serve", "--dir", "."] }
  }
}
```

If you already had an `engramdb` `mcpServers` entry, `setup` updates it in place. If you've **also** installed the plugin, `setup` detects that and skips writing the hooks/mcpServers to avoid duplicates — it only manages permissions.

## How the hooks behave

All context-injecting hooks group memories by epistemic class under `## Facts` / `## Observations` / `## Decisions` headers, ordered to fit the situation (session start: facts first; file edits: decisions first — override with `[hooks].class_order`). Decisions carry their rationale ("— because {premise}; revisit if {globs} changes"), observations their observed/verified dates. Task-scoped memories (`generality = task`) are hidden unless the session has declared the matching task (see below).

### `SessionStart`

Reads the event JSON from stdin and emits `additionalContext` listing high-criticality memories (criticality ≥ `--min-criticality`, default `0.6`), ranked with the `session_start` situation profile. The output is capped at ~2000 characters so it doesn't blow up the prompt; a standing reminder to record durable learnings is always appended, even when the store is empty. When task-scoped memories were hidden, a hint line says how many and how to surface them.

A typical session-start injection looks like:

```
[EngramDB] Key project memories:

## Facts (2):
- [convention] Memories always use TOML frontmatter; never YAML (source: shared/human)
- [hazard] LanceDB advisory lock is per-project; concurrent writes serialize (source: shared/human)

## Decisions (1):
- [decision] Use PgBouncer in production — because we need transaction-level pooling (source: shared/human)
```

### `PreToolUse (Read|Write|Edit)`

Reads the event JSON from stdin, extracts `tool_input.file_path`, and runs a `rank`-mode query with that path as the context and the `file_edit` situation — so decisions binding on the file come first, with hazards leading the facts group. Output is capped at `[hooks].prompt_context_budget` characters. The agent sees this just before the tool call runs. If the file path can't be relativized to the project root, the absolute path is used.

### `UserPromptSubmit`

Runs a `filter`-mode query with your prompt text as the query. It also infers a situation from the prompt: debugging-flavored wording ("error", "failing", "panic", …) ranks observations higher; design-flavored wording ("should we", "approach", "architecture", …) ranks prior decisions higher. Output is capped at `[hooks].prompt_context_budget` characters.

### `PostToolUse (Write|Edit|MultiEdit)`

After a file mutation, checks the edited path against every memory's watch paths (set via `--invalidated-by` / `invalidated_by`). On a match it warns:

```
[EngramDB]
⚠ this edit may invalidate memory a1b2c3d4 ('Retry logic assumes idempotent handlers') — verify it or update/invalidate it
```

Invalidated memories never warn. No output in the common case (no watch-path match).

### `SessionEnd`

Housekeeping only, no context output: clears the session's task mapping. When `[epistemic].demote_on_session_end = true` and the session had a declared task, it also demotes that task's task-scoped memories (same effect as `engramdb task complete`).

### `PreCompact`

Injects a short static reminder to store durable discoveries — decisions with their premise, hazards, verified observations — before the context window is compacted away.

## Declaring tasks: `task_current` / `task_complete`

Memories created with `generality = task` + an `origin_task` are scoped to a piece of work, not the whole project. Hooks hide them by default so one task's scratch findings don't pollute another session. To surface yours, declare what you're working on — via the MCP `task_current` tool, or `engramdb task current <NAME>` on the CLI. When the work is done, `task_complete` (MCP) or `engramdb task complete <NAME>` demotes the task's memories to fast decay so they age out on their own. The SessionStart hook tells the agent when task-scoped memories were hidden, so in practice the agent drives this flow itself.

## Troubleshooting

See [troubleshooting.md](./troubleshooting.md#claude-code).

## Disabling

To disable engramdb for a session without uninstalling:

```bash
# Plugin
/plugin disable engram@engramdb

# Manual setup
# Edit ~/.claude/settings.json and remove the engramdb hooks + mcpServers entries
```

Per-project disable: delete `<project>/.engramdb/` and the project's hooks won't trigger for that directory.
