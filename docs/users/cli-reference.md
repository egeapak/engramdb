# CLI Reference

`engramdb <command> --help` produces the same info inline.

## Global flags

These apply to every subcommand and must appear **before** the subcommand:

| Flag | Default | Description |
|------|---------|-------------|
| `--dir <DIR>` | current working dir | Project directory to operate on. |
| `--format <pretty\|json\|plain>` | `pretty` | Output format. |
| `--json` | off | Shorthand for `--format json`. |
| `--no-color` | off | Disable ANSI colors. |
| `-q, --quiet` | off | Suppress non-essential output. |
| `-v, --verbose` | off | Verbose output. |
| `--embedding-backend <auto\|onnx\|ollama>` | (from config) | Override the embedding backend for this invocation. |
| `--in-process` | off | Force in-process model loading — never contact the shared embedding daemon. Equivalent to `ENGRAMDB_IN_PROCESS=1`. |
| `--spawn-daemon` | off | Spawn the shared embedding daemon if it isn't already running, then route through it (by default the CLI is connect-only). |
| `--no-maintenance` | off | Skip the automatic main-worktree maintenance pass (orphan-project cleanup + quick store health check). Equivalent to `ENGRAMDB_DISABLE_AUTO_MAINTENANCE=1`. |

## Exit codes

All commands exit `0` on success and non-zero on error, so they can gate scripts and CI:

- `doctor` (and `doctor store` / `doctor validate`) exits non-zero when any check fails — advisory findings render as warnings and don't affect the exit code.
- `migrate` and `rollback` exit non-zero when the store is missing or when any per-file migration error occurred.

## `init` — initialize a store

```bash
engramdb init [--no-embeddings] [--template <path>]
```

Creates `<dir>/.engramdb/` and registers the project. Embeddings download on first use unless `--no-embeddings`.

## `add` — create a memory

```bash
engramdb add -t <type> [content] [flags...]
```

Positional argument: content. Alternatively `-c, --content <text>`, or `-i, --interactive`, or `-e, --editor`.

| Flag | Description |
|------|-------------|
| `-t, --type <T>` | `decision`, `convention`, `hazard`, `context`, `intent`, `relationship`, `debug`, `preference`. Required. |
| `-s, --summary <text>` | One-line summary (≤100 chars). Required — if omitted, you're prompted for it interactively in a terminal; non-interactive runs fail without it. |
| `-T, --title <text>` | Short title used in the on-disk filename. |
| `-c, --content <text>` | Content body. Alternative to positional. |
| `-p, --physical <glob>` | File path or glob. Repeatable. Default `/`. |
| `-l, --logical <dot.path>` | Logical scope. Repeatable. |
| `--tags <a,b,c>` | Tags. Comma-separated or repeated. |
| `--criticality <0..1>` | Importance score. Default 0.5. |
| `--confidence <0..1>` | Confidence. Default 0.8. |
| `--details <text>` | Extended details (lazy-loaded by default). |
| `--details-file <path>` | Read details from a file. |
| `--visibility <shared\|personal>` | Default `shared`. |
| `--supersedes <id,id>` | IDs this memory supersedes (closes their validity windows). |
| `--epistemic <fact\|observation\|decision>` | Epistemic class. Defaults from the type (context/convention/relationship/hazard → fact, debug → observation, decision/intent/preference → decision). |
| `--premise <text>` | Premise this memory depends on (e.g. "while we pin ort rc.12"). |
| `--invalidated-by <glob>` | Path/glob whose change invalidates this memory. Repeatable. |
| `--origin-task <name>` | Task/feature this memory was created for. |
| `--generality <project\|task>` | Default `project`. `task`-scoped memories are hidden from hook injection unless the session declared the matching task (see `task`). |
| `--valid-from <RFC3339>` | Backdate when the claim became true. |
| `--decay-strategy <none\|linear\|exponential\|step>` | Decay strategy. |
| `--decay-half-life <secs>` | Half-life for exponential decay. |
| `--decay-ttl <secs>` | TTL for any strategy. |
| `--decay-floor <0..1>` | Minimum decay factor. |
| `-i, --interactive` | Launch interactive prompts. |
| `-e, --editor` | Open `$EDITOR` for the content. |
| `--global` | Write to the global cross-project store instead of this project. |

## `get` — fetch a memory

```bash
engramdb get <id> [--full] [--raw] [--path] [--global]
```

`<id>` supports prefix matching. `--raw` emits the raw markdown file; `--path` prints the file path on disk.

## `query` — unified search

```bash
engramdb query --mode <rank|filter> [query] [flags...]
```

**Modes:**
- `--mode rank` — return memories sorted by composite score against the given context. No query signal required.
- `--mode filter` — require a positive signal: at least one of `--query`, `--logical`, `--path`, or `--tags`.

| Flag | Description |
|------|-------------|
| `--mode <rank\|filter>` | Required. |
| `[QUERY]` or `--query <text>` | Search text. Explicit flag wins over positional. |
| `-p, --path <path>` | Physical context for proximity scoring. |
| `-l, --logical <dot.path>` | Logical context (dot-notation). Repeatable. Scoring signal in `rank` mode; hard hierarchical filter in `filter` mode (`auth` matches `auth.oauth` and vice versa; siblings don't match). |
| `-t, --type <T>` | Filter by type. Repeatable. |
| `--tags <a,b,c>` | Filter by tags (OR within the list). |
| `--min-criticality <0..1>` | Drop memories below this. |
| `-n, --max-results <N>` | Default 10. |
| `--detail-level <summary\|content\|full>` | Output verbosity. |
| `--include-expired` | Include decayed/expired memories. |
| `--epistemic <fact\|observation\|decision>` | Filter by epistemic class. Repeatable. |
| `--situation <session_start\|file_edit\|debugging\|design_choice>` | Your situation — reweights classes via `[retrieval.scoring.situation]` (see [configuration.md](./configuration.md)). |
| `--include-invalidated` | Include invalidated memories (closed validity windows). |
| `--show-scores` | Print composite score per result. |
| `--include-global` | Merge global-store memories into results. |
| `--global` | Search the global store instead of the current project. |

See [agents/query-modes.md](../agents/query-modes.md) for when to use which.

## `list` — list memories

```bash
engramdb list [flags...]
```

| Flag | Description |
|------|-------------|
| `-t, --type <T>` | Filter by type. Repeatable. |
| `--epistemic <fact\|observation\|decision>` | Filter by epistemic class. Repeatable. |
| `--tags <a,b,c>` | Filter by tags. |
| `-s, --status <active\|needsreview\|challenged>` | Filter by status. |
| `--scope <text>` | Filter by physical or logical scope match. |
| `--sort <criticality\|created\|updated\|type>` | Sort field. Default `criticality`. |
| `-r, --reverse` | Reverse sort. |
| `-n, --limit <N>` | Cap output. |
| `--include-invalidated` | Include invalidated memories (closed validity windows). |
| `--global` | List the global store. |

## `update` — modify a memory

```bash
engramdb update <id> [flags...]
```

Same flags as `add`, plus:

| Flag | Description |
|------|-------------|
| `--tags-add <a,b>` | Add to existing tags. |
| `--tags-remove <a,b>` | Remove from existing tags. |
| `--status <active\|needsreview\|challenged>` | Set status manually. |
| `--clear-validity` | Clear the whole validity condition (premise / invalidated-by / origin-task / generality). |
| `--invalidate` | Close the validity window now: the memory *was* true but no longer is. Preferred over `delete` — history stays queryable via `--include-invalidated`. |
| `--superseded-by <id>` | Record which memory supersedes this one (only with `--invalidate`). |
| `--clear-invalidated` | Reopen a closed validity window (clears `invalidated_at` + `superseded_by`). |
| `-e, --editor` | Open the memory file in `$EDITOR`. |

For type/content/summary/scope/tags, the value **replaces** existing. Use `--tags-add` / `--tags-remove` for incremental tag changes.

## `delete` — remove a memory

```bash
engramdb delete <id> [-f] [--global]
```

`-f, --force` skips the confirmation prompt. For a memory that *was* true but no longer is, prefer `engramdb update <id> --invalidate` — it keeps the history queryable.

## `verify` — confirm a memory is still accurate

```bash
engramdb verify <id> [--global]
```

Stamps `verified_at = now` and clears a doctor-flagged needs-review status. Fact-class memories decay from their last verification, so verifying a fact refreshes its score; `doctor` suggests verification for observations unverified longer than `[epistemic].observation_review_days`.

## `task` — session task lifecycle

```bash
engramdb task current [NAME] [--session-id <id>] [--global]
engramdb task complete <NAME> [--global]
```

`task current NAME` declares the task this session is working on; with no `NAME` it reads the current declaration back. The session id comes from `--session-id` or the `CLAUDE_SESSION_ID` / `MCP_SESSION_ID` env vars. Declaring a task lets task-scoped memories (created with `--generality task` and a matching `--origin-task`) surface in hook injections; without a declaration they stay hidden from hooks (but remain reachable by explicit query).

`task complete NAME` marks the task finished and demotes its task-scoped memories to fast decay (memories with custom decay are left alone and reported separately).

## `challenge` — flag a memory

```bash
engramdb challenge <id> --evidence <text> [--source-file <path>] [--global]
```

Sets the memory's status to `Challenged` and records the evidence. Surface it later with `engramdb review --challenged-only`.

## `review` — interactive review

```bash
engramdb review [--challenged-only|--stale-only] [--stale-after-days [N]] [-t <type>] [--scope <text>] [--global]
```

Walks through memories one at a time and lets you keep, update, or delete each.

By default it lists flagged memories (challenged / needs-review). `--stale-after-days` adds the **recency trigger**: active memories not updated in more than `N` days are folded in too (a bare `--stale-after-days` uses the 90-day default). Every keep/update resets a memory's clock, so this surfaces knowledge nobody has revisited in a while for you to confirm or retire.

## `stats` — store statistics

```bash
engramdb stats [--all-projects] [--global] [--daemon]
```

| Flag | What you see |
|------|---------------|
| (no flag) | Counts by type/scope/status for the current project. |
| `--all-projects` | Cross-project runtime telemetry breakdown. |
| `--global` | Stats for the global store. |
| `--daemon` | Embedding-daemon request metrics (see [daemon.md](./daemon.md)). |

## `doctor` — health check

```bash
engramdb doctor [store|validate] [--fix] [--yes] [--global]
```

Without a subcommand: full environment diagnostics (paths, embedding backend, daemon, model files, store consistency).

| Flag / subcommand | Description |
|-------------------|-------------|
| `store` | Fast project-scoped check (index vs disk only). Use it as a CI/script smoke test. |
| `validate` | Load each downloaded model and run a test inference to confirm it works. |
| `--fix` | Offer to fix detected issues (reindex, download model, prune registry, init). Prompts on a terminal; in non-interactive contexts pair with `--yes`. |
| `--yes` | Apply fixes without prompting (use with `--fix`; required to fix in non-TTY contexts). |
| `--global` | Check the global cross-project store instead of the current project. |

`doctor` exits non-zero when any check fails (see [Exit codes](#exit-codes)).

## `gc` — garbage collect

```bash
engramdb gc [--confirm] [--threshold <N>] [--global]
```

Default is dry-run. Add `--confirm` to actually delete. `--threshold` overrides the config-driven default (`thresholds.gc`).

## `compress` — list compression candidates

```bash
engramdb compress [--scope <text>] [--threshold <0..1>] [--global]
```

Reports candidates only. The actual merge happens via the MCP `compress_apply` tool (it needs an agent to write the summary).

## `reindex` — rebuild vectors and index

```bash
engramdb reindex [--embeddings-only|--index-only] [--global]
```

| Flag | What runs |
|------|-----------|
| (no flag) | Re-embed everything + rebuild the LanceDB index. |
| `--embeddings-only` | Re-embed only. |
| `--index-only` | Rebuild the index without re-embedding. |

## `migrate` / `rollback` — memory format migrations

```bash
engramdb migrate [--dry-run] [--global]
engramdb rollback --target-version <N> [--dry-run] [--global]
```

Move memory files between format versions. Both exit non-zero when the store is missing or when any per-file error occurred, so they can gate scripts (see [Exit codes](#exit-codes)).

## `serve` — start the MCP server

```bash
engramdb serve [--transport stdio|sse] [--port <N>]
```

`stdio` (default) is what Claude Code uses. `sse` runs an HTTP streaming server on `--port`. The plugin's `mcpServers` entry runs `engramdb serve --dir .`.

## `daemon` — shared embedding daemon

```bash
engramdb daemon run     [--socket <path>] [--idle-timeout <secs>]
engramdb daemon status  [--socket <path>]
engramdb daemon stop    [--socket <path>]
engramdb daemon restart [--socket <path>] [--idle-timeout <secs>]
```

Normally auto-spawned by MCP. See [daemon.md](./daemon.md).

## `setup` — Claude Code integration

```bash
engramdb setup [--global] [--no-plugin] [--dry-run]
```

| Flag | Effect |
|------|--------|
| (none) | Writes to `<project>/.claude/`. |
| `--global` | Writes to `~/.claude/`. |
| `--no-plugin` | Global only. Forces direct `settings.json` writes instead of using the marketplace plugin. |
| `--dry-run` | Prints the diff without writing. |

See [claude-code.md](./claude-code.md).

## `hook` — Claude Code hook handlers

```bash
engramdb hook pre-tool-use                            # PreToolUse for Read/Write/Edit
engramdb hook session-start [--min-criticality <0..1>] # SessionStart, default 0.6
engramdb hook user-prompt-submit                      # UserPromptSubmit: prompt-relevant memories
engramdb hook post-tool-use                           # PostToolUse for Write/Edit/MultiEdit: watch-path warnings
engramdb hook session-end                             # SessionEnd: housekeeping, no output
engramdb hook pre-compact                             # PreCompact: store-your-memories reminder
```

Invoked by Claude Code, not manually. See [claude-code.md](./claude-code.md#how-the-hooks-behave) for what each hook does.

## `projects` — registry management

```bash
engramdb projects info                          # current project info (default)
engramdb projects list                          # all registered projects with hierarchy
engramdb projects stats                         # cross-project aggregate stats
engramdb projects delete <project_id> [-f] [--cascade]
engramdb projects link <child_id> --parent <parent_id>
engramdb projects unlink <project_id>
engramdb projects prune [-f]
```

See [projects-and-worktrees.md](./projects-and-worktrees.md).

## `completions` — shell completions

```bash
engramdb completions <bash|zsh|fish|powershell|elvish>
```

Emits the completion script on stdout.
