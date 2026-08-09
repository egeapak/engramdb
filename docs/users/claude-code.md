# Claude Code Integration

Once wired up, EngramDB exposes its MCP tool surface and runs six hooks automatically:

- **SessionStart** injects high-criticality memories as `additionalContext`, grouped by epistemic class.
- **PreToolUse (Read|Write|Edit)** surfaces memories relevant to the file being touched.
- **UserPromptSubmit** surfaces memories relevant to the prompt you just submitted.
- **PostToolUse (Write|Edit|MultiEdit)** warns when an edit touches a path a memory is watching.
- **SessionEnd** does task housekeeping and keeps a compressed copy of the session transcript (no context output).
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

No context output. It clears the session's task mapping, and when
`[epistemic].demote_on_session_end = true` and the session had a declared task,
demotes that task's task-scoped memories (same effect as `engramdb task
complete`). It then keeps a copy of the session transcript. Everything here is
best-effort — a failure is logged and swallowed, never blocking session
teardown — and the hook does nothing at all in a directory that was never
`engramdb init`ed, which matters because the plugin registers SessionEnd
machine-wide.

**The transcript copy.** Claude Code prunes its own transcripts, so a session
becomes unharvestable once its file is gone — and any memory derived from it
loses its evidence. Session end is the last moment the file is reliably still
there, so SessionEnd writes a zstd-compressed copy to

```
<engramdb data dir>/projects/<root-project-id>/transcripts/
```

deliberately **outside** your repository, never under `.engramdb/`, which gets
committed. The id is the **root** of the project's hierarchy, so a git
worktree's copies land beside the main checkout's rather than in a directory of
their own.

It is a **verbatim copy of the whole conversation**: your prompts, the
assistant's replies, and full tool output — in practice that includes command
output, file contents, and anything pasted into the chat. It is kept verbatim
on purpose: it is the evidence a challenged memory resolves back to, and a
reduction taken at copy time could never be improved on later. It stays on your
machine, owner-readable only; nothing is transmitted.

The same run appends one line to the project's harvest ledger, recording where
the copy landed and its size. The ledger is an append-only JSONL log at

```
<project>/.engramdb/state/harvest_ledger.jsonl
```

under the root project — one line per state change, holding session ids,
review decisions, timestamps and the pointer to the copy. **No conversation
content is in it**; unlike the copy it is repo-adjacent and meant to be
committed. A session the hook collects is recorded as `unreviewed`, which is
what later lets `/engram:harvest` offer it.

Last, the run does a retention sweep over the copies. The bounds, all under
`[harvest]` in `.engramdb/config.toml`:

| Setting | Default | Effect |
| --- | --- | --- |
| `archive_retention_days` | `365` | copies older than this are deleted (max `3650`) |
| `archive_max_bytes` | `2147483648` (2 GiB) | total budget; oldest evicted first |
| `archive_max_transcript_bytes` | `16777216` (16 MiB) | a transcript larger than this is not copied (the sweep still runs) |

Evicted files are cleared from the ledger too, so it never advertises an export
that cannot succeed. A copy cited as a memory's evidence (see
[`harvest mark --memory`](./cli-reference.md#harvest--mine-past-claude-code-sessions))
is **pinned** and is not evicted by either bound.

**To turn it off**, set `archive = false` under `[harvest]`:

```toml
[harvest]
archive = false
```

The rest of SessionEnd (task housekeeping) still runs. To clear what has
already accumulated, `engramdb harvest ledger prune --apply`; to pull one copy
back out, `engramdb harvest ledger export <session-id>`; to delete one copy and
its record, `engramdb harvest ledger rm <session-id>`.

### `PreCompact`

Injects a short static reminder to store durable discoveries — decisions with their premise, hazards, verified observations — before the context window is compacted away.

## Declaring tasks: `task_current` / `task_complete`

Memories created with `generality = task` + an `origin_task` are scoped to a piece of work, not the whole project. Hooks hide them by default so one task's scratch findings don't pollute another session. To surface yours, declare what you're working on — via the MCP `task_current` tool, or `engramdb task current <NAME>` on the CLI. When the work is done, `task_complete` (MCP) or `engramdb task complete <NAME>` demotes the task's memories to fast decay so they age out on their own. The SessionStart hook tells the agent when task-scoped memories were hidden, so in practice the agent drives this flow itself.

## Slash commands: `/engram:reflect` and `/engram:harvest`

Both ship with the plugin (they are markdown command files, so a hooks-only
`engramdb setup` install does not get them).

`/engram:reflect` reviews the session you are **in**: it asks the agent to
capture anything durable about the project, the environment, or your
preferences before handing back.

`/engram:harvest` reviews sessions that are already **over**. Claude Code
keeps a transcript of every session on disk; this command reads the ones
belonging to the current project — **including its git worktrees**, which
file transcripts under their own paths but share the project's memory store
— and mines them for knowledge that was never captured. Use it to backfill a
project you have been working on since before EngramDB was installed.

The flow is deliberately gated: the agent lists **every** candidate memory
with the evidence behind it and waits for your approval before saving
anything. Sessions that hold nothing worth keeping are reported as such —
that is a normal outcome, not a failure — and are recorded as reviewed so
they are not re-read next time.

It is backed by the `engramdb harvest` CLI command, which does the
transcript reading and digesting; see
[cli-reference.md](./cli-reference.md#harvest--mine-past-claude-code-sessions)
for the flags, including `--since`, `--all-projects`, and the `--max-chars`
budget.

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
