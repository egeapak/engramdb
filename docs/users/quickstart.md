# Quickstart

From install to a working store in five minutes. Prerequisites: [installation.md](./installation.md).

## 1. Initialize a store

From the root of any project:

```bash
engramdb init
```

Creates `<project>/.engramdb/` and registers the project. The embedding model downloads on first `add` / `query`. Flags: `--no-embeddings` (skip), `--template <path>` (start from a template).

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
- `--premise "<condition>"` — what the memory depends on (e.g. `--premise "while we pin ort rc.12"`); injected context shows it as "— because ...".
- `--invalidated-by <glob>` — paths whose change should trigger a re-check; when a Claude Code hook sees an edit under a watched path, it warns that the memory may be stale.

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
engramdb get <id>                          # full details (ID prefix-match OK)
engramdb get <id> --raw                    # raw markdown file
```

## 5. Update, challenge, delete

```bash
engramdb update <id> --criticality 0.9 --tags-add migrations
engramdb challenge <id> --evidence "ADR-007 reverses this; we moved back to SQLite"
engramdb verify <id>          # confirm it's still accurate (refreshes fact decay)
engramdb update <id> --invalidate   # it WAS true but no longer is; history stays queryable
engramdb delete <id>          # asks for confirmation; use --force to skip
```

For something that used to be true, prefer `--invalidate` over `delete`: the memory drops out of queries but remains inspectable with `--include-invalidated`.

## 6. Hook it into Claude Code

```bash
engramdb setup           # project-scoped: writes to <project>/.claude/
engramdb setup --global  # writes to ~/.claude/
```

Or install the plugin — see [claude-code.md](./claude-code.md) for the full picture.
