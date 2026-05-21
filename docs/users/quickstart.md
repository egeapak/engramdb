# Quickstart

This walks you through creating your first EngramDB store, adding a memory, and querying it. Five minutes from a fresh install to a working store.

If you haven't installed yet, see [installation.md](./installation.md).

## 1. Initialize a store

From the root of any project:

```bash
engramdb init
```

This creates `<project>/.engramdb/` with `manifest.toml`, an empty `memories/` directory, and a `config.toml`. It also registers the project in the global registry and downloads the default embedding model the first time you actually need vectors (e.g. on the first `add` or `query`).

Flags:
- `--no-embeddings` — skip embedding initialization (storage works, semantic search is degraded).
- `--template <path>` — start from a config template.

## 2. Add a memory

```bash
engramdb add \
  --type decision \
  --title "Use PostgreSQL for persistence" \
  --summary "Chose PostgreSQL over SQLite for concurrent write support" \
  --physical "src/db/**" \
  --logical "database.connection" \
  --tags db,architecture \
  --criticality 0.8 \
  "We picked PostgreSQL because the app needs many concurrent writers, \
   and SQLite serializes all writes. Migration path: see ADR-007."
```

The trailing positional argument is the content. You can also use `--content "..."` explicitly, or `--interactive` / `--editor` to compose in `$EDITOR`.

Other fields you'll commonly set:
- `--type <T>` — one of `decision`, `convention`, `hazard`, `context`, `intent`, `relationship`, `debug`, `preference`. See [memory-model in the agents docs](../agents/memory-model.md) for what each type means.
- `--criticality 0.0..1.0` — how important this memory is. High-criticality memories are surfaced at session start and survive GC.
- `--physical <glob>` — file paths or globs this memory applies to. Repeatable. Default is `/` (whole project).
- `--logical <dot.path>` — logical scope (e.g. `auth.oauth`, `database.migrations`). Repeatable.
- `--visibility shared|personal` — `personal` keeps it under the global data dir, not in the project tree (useful when you don't want to commit a memory).

## 3. Query

EngramDB has one query command with two modes:

```bash
# Filter mode: requires a positive signal (query / path / logical / tags)
engramdb query --mode filter "postgres"

# Rank mode: rank everything by relevance to a context
engramdb query --mode rank --path src/db/connection.rs

# Combine: filter by query and rank by path context
engramdb query --mode rank --query "concurrent writes" --path src/db/connection.rs
```

Common flags:
- `--max-results 10` — cap results.
- `--detail-level summary|content|full` — how much to show.
- `--type decision --tags db` — filter by type / tags before scoring.
- `--min-criticality 0.5` — drop low-importance memories.
- `--show-scores` — print the composite relevance score for each result.

For when to use each mode, see [agents/query-modes.md](../agents/query-modes.md).

## 4. List and inspect

```bash
engramdb list                              # all memories
engramdb list --type hazard                # hazards only
engramdb list --sort created --reverse     # newest first
engramdb get <id>                          # full details for one memory (prefix-match OK)
engramdb get <id> --raw                    # raw markdown file
```

ID prefix matching means you rarely need the full UUID — `engramdb get abc` works if there's no ambiguity.

## 5. Update, challenge, delete

```bash
engramdb update <id> --criticality 0.9 --tags-add migrations
engramdb challenge <id> --evidence "ADR-007 reverses this; we moved back to SQLite"
engramdb delete <id>          # asks for confirmation; use --force to skip
```

`challenge` doesn't delete the memory; it marks it `Challenged` for `engramdb review` to surface later.

## 6. Hook it into Claude Code

```bash
engramdb setup                  # writes hooks + MCP to <project>/.claude/
# or
engramdb setup --global         # writes to ~/.claude/
```

After that, Claude Code automatically:
- runs `engramdb hook session-start` at the start of every session (injects high-criticality memories);
- runs `engramdb hook pre-tool-use` when the agent reads/writes/edits a file (surfaces relevant memories);
- starts `engramdb serve` as an MCP server so the agent can query, create, and challenge memories directly.

Or use the plugin — see [claude-code.md](./claude-code.md).

## What's next

- **Configure**: tune scoring, GC thresholds, daemon, NLI/reranker — see [configuration.md](./configuration.md).
- **Browse all commands**: [cli-reference.md](./cli-reference.md).
- **Understand the daemon**: [daemon.md](./daemon.md).
- **Worktrees and the global store**: [projects-and-worktrees.md](./projects-and-worktrees.md).
