# EngramDB Plugin for Claude Code

Persistent memory for your coding agent. This plugin gives Claude Code the ability to remember decisions, hazards, conventions, and context about your codebase across sessions.

## Prerequisites

EngramDB must be installed and available on your PATH:

```bash
cargo install --git https://github.com/egeapak/engramdb engram-cli
```

(The `engramdb` binary ships in the `engram-cli` workspace crate.)

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

A full MCP server (`engramdb serve`) starts automatically, providing 27 tools for memory and project management:

`query`, `create`, `get`, `list`, `update`, `delete`, `challenge`, `review`, `resolve`, `verify`, `task_current`, `task_complete`, `stats`, `config`, `doctor`, `gc`, `reindex`, `compress_candidates`, `compress_apply`, `projects_list`, `projects_info`, `projects_link`, `projects_unlink`, `harvest_list`, `harvest_show`, `harvest_mark`, `harvest_ledger`

### Shared embedding daemon

Each Claude Code session runs its own `engramdb serve` process. Rather than
every session loading its own copy of the embedding (and optional NLI /
reranker) models, EngramDB **auto-spawns a single shared daemon** that loads
each model once machine-wide and serves inference over a per-user Unix socket.
It is fully automatic — no setup, nothing to run — and self-exits when idle; the
next session that needs it spawns a fresh one. If it can't start, sessions fall
back to loading models in-process, so nothing ever fails because of it.

You normally never touch it, but `engramdb daemon status|stop|restart` and
`engramdb stats --daemon` are available, `engramdb doctor` reports its state,
and it can be disabled with `enabled = false` under `[daemon]` in
`.engramdb/config.toml`.

### Hooks

- **SessionStart** — injects high-criticality memories (hazards, active decisions) into the conversation when a session begins, grouped by epistemic class
- **PreToolUse (Read/Write/Edit)** — surfaces relevant memories as context when the agent touches files
- **UserPromptSubmit** — surfaces prompt-relevant memories, inferring your situation (debugging vs. design) to reweight what appears
- **PostToolUse (Write/Edit/MultiEdit)** — warns when an edit touches a path some memory declared as its invalidation trigger
- **SessionEnd** — housekeeping: clears the session's task mapping (and optionally demotes task-scoped memories), and archives a compressed copy of the session transcript so `/engram:harvest` can still mine it later

> **What the transcript archive stores.** Claude Code prunes its own
> transcripts, so a session becomes unharvestable once its file is gone.
> SessionEnd therefore keeps a zstd-compressed copy at
> `<engramdb data dir>/projects/<project-id>/transcripts/` — deliberately
> **outside** your repository, never under `.engramdb/`, which gets committed.
>
> It is a verbatim copy of the whole conversation: your prompts, the
> assistant's replies, and full tool output — in practice that includes
> command output, file contents, and anything pasted into the chat. It stays
> on your machine, owner-readable only; nothing is transmitted. Bounded by
> age (365 days), total size (2 GiB, oldest evicted first), and per-file size
> (16 MiB).
>
> Turn it off with `archive = false` under `[harvest]` in
> `.engramdb/config.toml`. `engramdb harvest ledger prune --apply` deletes
> what has accumulated; `engramdb harvest ledger export <session-id>`
> restores one.
- **PreCompact** — reminds the agent to store durable discoveries before context compaction

### Slash commands

- **`/engram:reflect`** — reflect on the session you are *in* and persist
  durable project / environment / preference learnings.
- **`/engram:harvest`** — mine sessions that are already *over*. Reads past
  transcripts, proposes what is worth remembering, lists every candidate for
  your approval, and saves only what you accept. Covers this project and its
  worktrees by default.

Use `harvest` to backfill a project whose earlier sessions were never
captured; use `reflect` at the end of the session you are in.

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
  --summary "Schema changes require a reindex to rebuild embeddings" \
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
