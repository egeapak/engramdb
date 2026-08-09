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
| `-s, --summary <text>` | One-line summary (≤200 chars by default, configurable via `[content].summary_max_chars`). Required — if omitted, you're prompted for it interactively in a terminal; non-interactive runs fail without it. |
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
| `--fix` | Offer to fix detected issues (reindex, download the embedding model, prune the registry, re-key a project whose ID drifted, init). Prompts on a terminal; in non-interactive contexts pair with `--yes`, which is also what lets the epistemic checks flag memories for review. Exits on the post-fix state; declining every fix, or finding none, still exits on the checks. Without `--yes` off a terminal it only lists the fixes and exits 0. |
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
engramdb reindex [[--embeddings-only|--index-only] [--global] | --archive-only]
```

| Flag | What runs |
|------|-----------|
| (no flag) | Re-embed everything + rebuild the LanceDB index. |
| `--embeddings-only` | Re-embed only. |
| `--index-only` | Rebuild the index without re-embedding. |
| `--archive-only` | Rebuild the **conversation** search rows from the stored transcript copies — the copies, not the live transcripts, even where Claude Code still has one. Touches no memory; see [`harvest search`](#harvest--mine-past-claude-code-sessions). Curated summaries are preserved — they are the one thing a rebuild cannot recreate. This is also the remediation when `[embeddings].dimensions` changed under an existing conversation index: the table's vector width is fixed at creation, so it is recreated at the new width (carrying the summaries across) before the rows are rebuilt. Rejected alongside any other flag, `--global` included: conversation rows live in the **root project's** index, which has no global-store counterpart. |

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
engramdb hook session-end                             # SessionEnd: housekeeping + transcript copy, no output
engramdb hook pre-compact                             # PreCompact: store-your-memories reminder
```

Invoked by Claude Code, not manually. See [claude-code.md](./claude-code.md#how-the-hooks-behave) for what each hook does.

## `projects` — registry management

```bash
engramdb projects info                          # current project info (default)
engramdb projects list [--group auto|always|none]  # all registered projects as a tree
engramdb projects discover [PATH] [--yes] [--dry-run]  # adopt unregistered projects
engramdb projects repair [-f] [--no-index]      # re-key a project whose ID drifted
engramdb projects stats                         # cross-project aggregate stats
engramdb projects delete <project_id> [-f] [--cascade] [--purge]
engramdb projects link <child_id> --parent <parent_id>
engramdb projects unlink <project_id>
engramdb projects prune [-f]
```

`projects list` renders a directory tree: projects are grouped under
filesystem-folder headers and worktree sub-projects nest under their real
parent (marked `↳`). `--group` sets the grouping for one run, overriding the
`[cli].project_list_grouping` config default:

- `auto` (default) — a folder header only for directories with two or more
  projects; a lone project renders inline on a full-path line.
- `always` — a header above every folder; project rows show just the basename.
- `none` — a flat list of full-path rows, no headers.

Worktree nesting and path sorting apply in every mode. `--json` output is
unaffected by `--group`: it stays a flat array carrying `parent_project_id`.

`projects delete` removes a *registration* and reclaims the data directory
behind it. It keeps that directory whole whenever it still holds personal
memories, unless `--purge` is passed — a project ID derived from a git remote is
shared by every clone of that remote on the machine, and the registry records
only one of them, so deleting one checkout's registration can otherwise destroy
another checkout's only copy. `--purge` is the explicit "I mean it"; without
`-f` the prompt spells out which of the two you are about to do.

`--force` is required in JSON mode (the command never prompts there), and it
emits one object: `{deleted, project_id, project_path, purge,
global_data_removed, retained_with_personal[], cascaded_ids[]}`. Refusing to act
— a project with sub-projects and no `--cascade` — exits non-zero rather than
reporting a delete that did not happen.

`projects prune` drops registry entries whose project directory is gone and
reclaims data directories no registration answers to. It never deletes personal
memories: a data directory holding `personal/memories/*.md` is left whole, index
included — it is being kept precisely because something unregistered may still
be using it, and wiping that copy's index would leave a healthy project silently
unsearchable. Those directories are
listed as `retained_with_personal` (in JSON and in the human summary) so a clean
run is not mistaken for "everything was reclaimed". The same rule applies to the
unattended maintenance pass, which runs prune for you.

`--force` is required in JSON mode whether or not there is anything to prune —
JSON is machine-consumed and never prompts, so the contract depends on the flags
alone. The one emitted object carries `stale_removed`, `stale_ids[]`,
`orphans_removed`, `orphan_ids[]`, `hierarchy_cleared[]`, and
`retained_with_personal[]`, at their zero values when nothing was pruned.

### `projects discover`

Walks a directory tree for `.engramdb/` projects the registry doesn't know
about and offers to register and index each one. Useful after cloning a repo
that carries its memories, restoring from a backup, or losing `registry.json` —
those projects exist on disk but are invisible to `projects list`, `projects
stats`, and every cross-project surface until they are registered.

| Flag | Behavior |
| --- | --- |
| `PATH` | Directory to scan. Defaults to the project directory this invocation resolved to — note that inside a linked git worktree that is the **main** checkout, not the cwd. |
| `--max-depth <N>` | Maximum depth to descend below the scan root (default 6). The summary says when a subtree was cut off. |
| `--hidden` | Also descend into dot-directories. |
| `--follow-symlinks` | Follow directory symlinks (the walk visits each canonical path once either way). |
| `-y`, `--yes` | Register everything found without prompting. Required in JSON mode unless `--dry-run` is passed (JSON is machine-consumed, so the command never prompts). |
| `--dry-run` | Report what would be registered without registering anything. (The usual automatic maintenance pass still runs, as it does for every command.) |
| `--no-index` | Register only. Memories stay unsearchable until you run `engramdb reindex`. |

Without `--yes` you are asked once per project, so a scratch clone can be
declined while a real checkout is adopted. Accepting registers the project
(idempotent — an existing `manifest.toml` / `config.toml` is never overwritten)
and rebuilds its index from the on-disk `.md` files, with an indicatif progress
bar over the batch.

Directories that can't hold a project root are never descended into
(`node_modules`, `target`, `.git`, `vendor`, `dist`, `build`, virtualenvs, …),
and engramdb's own global/group stores are never offered. Three kinds of project
are reported but never registered — as a warning in human output, and in the
`skipped[]` array in JSON:

- **Shared project ID** — the ID is already claimed by a different checkout
  that still exists, either in the registry or earlier in the same scan. Two
  clones of one git remote hash to the same ID and would share a single index.
- **Linked git worktree** — worktrees are sub-projects, not roots: engramdb
  routes their memory operations to the main checkout automatically. Adopting
  one would create a second owner of the same memory files. A worktree already
  linked as a sub-project (the steady state) reports as registered, not here.
- **Stale project ID** — the path *is* registered, but under an ID it no
  longer hashes to. Adopting would leave two registry entries for one path; the
  fix is `projects repair` (below).

Exit status is non-zero if any project failed to register; the report is still
emitted first, so you can see which ones. Directories that can't be read
(permissions, dead mounts) are skipped and counted, and the summary says so —
"no unregistered projects found" after a partial scan is not the same claim as
"there are none".

JSON mode emits exactly one of two objects, both carrying `root`,
`scanned_dirs`, `depth_limited`, `dry_run`, and `skipped[]`
(each `{path, project_id, reason, owner}` where `reason` is
`shared_project_id`, `git_worktree`, or `stale_project_id`; `owner` is the
conflicting checkout for the first two and `null` for the third, which carries
`registered_id` — the stale ID — instead).

- `--dry-run` adds `candidates[]` (each `{path, project_id, memory_count}`) and
  `already_registered[]`.
- A real run adds `unreadable_dirs`, `no_index`, `found_unregistered`,
  `registered[]` (each
  `{path, project_id, indexed, embedded, warnings[]}`), `declined[]`, and
  `errors[]` (each `{path, error}`).

Arrays are empty rather than absent when nothing was found or everything was
declined, so the shape never varies with the outcome. Under `--no-index`,
`indexed` and `embedded` are `null` — "not rebuilt", as distinct from `0`,
which means the rebuild ran and found nothing.

### `projects repair`

Re-keys a project whose ID drifted out from under its registry entry.

`compute_project_id` hashes the git remote when there is one and falls back to
the path when there isn't, so running `engramdb init` **before**
`git remote add origin …` permanently changes the project's ID. The registry
keeps the old one; every live operation uses the new one. The symptoms are all
silent — memories vanish from `list`/`query` (the live ID's index is empty even
though the `.md` files are untouched), group subscriptions detach, and personal
memories become invisible.

```bash
engramdb projects repair            # show the blast radius, then confirm
engramdb projects repair -f         # skip the prompt (required in JSON mode)
engramdb projects repair --no-index # re-key only; run `engramdb reindex` later
```

It migrates the registry entry in place — preserving group subscriptions and
worktree parent links, which a re-registration would silently drop — carries
the personal memories over to the live data directory, re-points any
sub-projects at the new ID, and rebuilds the index. Running it on a consistent
project is a no-op, so it is safe to re-run.

A project that drifted more than once (`init` → add a remote → change the
remote) has several stale IDs. Every one of them is collapsed into a single
entry, and every one's personal memories are carried across — the directory you
were writing into just before the last drift is not the first stale ID.

It **never deletes anything**: personal memories are *copied* to the live data
directory and the old one is left in place. An unregistered sibling clone of the
same remote shares that directory and is structurally invisible to the registry,
so no check can prove it is yours alone. `projects prune` later reclaims the
rebuildable index inside it, but never the personal memories. Files that can't
be read or parsed — on either side — are left alone and counted in
`personal_skipped`; that includes a live file carrying the same memory ID that
this binary can't parse, which is never replaced by an older copy.

It refuses outright when a registry row at **another path** already holds the
live ID (two rows sharing one ID resolve to whichever comes first, so the repair
would report success while the symptom persisted), and when run inside a linked
git worktree (worktrees route to the main checkout and are never re-keyed). A
sibling clone still answering to the *old* ID is fine: that is the normal state
for two clones of one remote, and nothing here writes to or deletes the shared
directory.

JSON mode emits one object: `{"repaired": false, "reason": "nothing_to_repair"}`
when the registration is consistent, otherwise `{repaired, path, old_id,
old_ids[], new_id, personal_migrated, personal_superseded, personal_skipped,
removed_duplicate_entry, reparented_children[], old_data_dir, old_data_dirs[],
no_index, indexed, embedded, warnings[], index_error}`. `--force` is required in JSON mode
regardless of whether this project is drifted. If the re-key succeeds but the
index rebuild fails, the document is still emitted (with `index_error` set) and
the command then exits non-zero — retrying `repair` would report nothing to do,
so run `engramdb reindex`.

`engramdb doctor` reports the same condition as a `Project identity` warning,
and `doctor --fix` offers this repair.

See [projects-and-worktrees.md](./projects-and-worktrees.md).

## `harvest` — mine past Claude Code sessions

```bash
engramdb harvest list [--since 7d] [-n N] [--include-harvested]
                      [--include-empty] [--all-projects] [--exclude-session ID]
engramdb harvest show <session_id> [--max-chars N] [--include-thinking]
                      [--include-sidechains] [--no-tools] [--all-projects]
engramdb harvest mark <session_id> [[--memory <id>]... | --defer] [--note <text>]
                      [--all-projects] [--summary "<text>"]
engramdb harvest index [<session_id> | --all] [--force]
engramdb harvest search <query> [-n N] [--since 30d] [--all-projects]
engramdb harvest summary <session_id> [<text> | --editor | --from-file <path>]
engramdb harvest reset <session_id>
engramdb harvest ledger list [--decision harvested|skipped|deferred|unreviewed]
                             [--stage collected|indexed|compressed] [--with-archive]
engramdb harvest ledger show <session_id>
engramdb harvest ledger export <session_id> [-o <path>]
engramdb harvest ledger rm <session_id> [--archive-only] [--unpin] [--force]
engramdb harvest ledger prune [--older-than 90d] [--max-bytes N] [--apply]
```

Reads the transcripts Claude Code writes to
`~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` and presents them for
review. This command only *presents* sessions — it never writes a memory.
The `/engram:harvest` slash command drives it and does the saving.

**Scope.** `list` and `show` cover the **root** of the current project's
hierarchy and every project registered under it — so running from a git
worktree also covers the main checkout and the sibling worktrees, not just
this one. A worktree files its transcripts under its own path but shares the
main checkout's memory store, so its sessions are harvested alongside the main
ones. Attribution uses the `cwd` recorded inside each
transcript, not the directory name — that name is a lossy encoding of the
path and can collide. `--all-projects` ignores scoping entirely.

**Digests, not raw transcripts.** `show` prints a budgeted digest: prompts
and assistant prose verbatim, each tool call as a single line with its
target and success/failure, results as a one-line preview. A raw transcript
is ~99% tool payload, so digesting is what makes review affordable — a
1.2 MB transcript renders to ~63 KB, and a 2.9 MB one to ~60 KB — the
default budget is a ceiling against a pathological session, not a routine
constraint. When content had
to be dropped to fit, the header says `partial digest` and names what went;
raise `--max-chars` to see more. One entry there does *not* respond to a larger
budget: a single event is capped at 1,500 characters before the budget is even
consulted, so one pasted stack trace cannot cost a whole session its slot. The
digest reports those separately as `N long events each cut to 1500 chars`
(`capped_events` in `--json`); `harvest ledger export` is the route to the full
text.

A second entry is beyond any budget's reach: a single JSONL record larger than
4 MiB — in practice a pasted screenshot, which Claude Code embeds as base64 —
is dropped by the parser before the digest is assembled. The header reports
`N records over 4 MiB dropped by the parser` (`skipped_records` in `--json`)
and the turn counts printed above it are then lower bounds, since the dropped
records never became turns. Re-running with a larger `--max-chars` returns the
identical text; `harvest ledger export` is again the only route to the
original.

Session ids accept unique prefixes. `--json` (or any non-TTY invocation)
emits the structured event list alongside the rendered markdown.

**Searching past conversations.** `harvest search` answers "did we ever
discuss X" and "why did the build break in July" over the conversations that
have been indexed.

Indexing rides the throttled maintenance pass, which runs at most once per
`[maintenance] interval_secs` (**21600**, six hours). Two things become due: a
session a human settled with `mark`, at once, and a session **nobody** reviewed,
once it is older than `[harvest] index_after_hours` (**24**). That second
trigger is the point — if search only found what you had already read, it would
only find what you no longer need. One pass embeds at most
`[harvest] index_batch` (**25**) conversations, newest first, so a machine with
years of history fills in over successive passes.

**The pass only indexes where an embedding provider is already loaded, which
means the MCP server** — running the CLI does not build one, and its maintenance
pass skips indexing entirely (the same way it skips consolidation). So on a
machine that has never run `engramdb serve`, nothing is indexed until you ask:
`harvest index <session_id>` or `harvest index --all` does it by hand, and is
the only route on a CLI-only setup. `[harvest] index = false` turns the
automatic pass off altogether.

Each conversation gets two vectors. `digest_vec` is always present and is
embedded from the deterministic, code-generated reduction of the session:
prompts and assistant prose, plus every **failed** tool call and its error
text, with successful tool calls dropped because they dilute the vector.
`summary_vec` is present only once someone has written a curated summary
(`harvest summary`, or `mark --summary`). Both are queried and the better score
wins, with an exact tie broken toward the summary — a human wrote it, so a
match there is higher precision. Editing a summary re-embeds only the summary;
the digest vector is untouched.

Search returns session ids and metadata, not conversation text — pass an id to
`harvest show` to read one. A hit marked `partial` is a session whose tail was
never embedded (the indexed text is budgeted to what the embedding model can
actually read), so a *miss* against it is not evidence the topic was absent.
`--since` narrows the candidates *before* the nearest-neighbour cut rather than
trimming its output, so a window still returns its best `-n` matches however
many older conversations rank above them. A session with no recorded end time
is excluded by `--since`, the same rule `harvest list` applies: it cannot be
shown to fall inside the window.

`harvest index` is idempotent: each row records the checksum of the exact text
behind its vector, so re-running costs one hash and no embedding call.
`--force` re-embeds anyway. `reindex --archive-only` rebuilds every row from
the stored transcript copies — the payoff of keeping those copies verbatim, and
the reason the digest vector is never derived from an agent's prose: an
agent-authored summary is not regenerable by code, so it is stored and
separately embedded but never what recall depends on. A rebuild preserves it.

**The ledger.** `mark` records that a session was reviewed so it is not
offered again, and *must* be used even when a session yielded nothing —
a zero-yield session leaves no other trace, so without a mark it is re-read
on every future harvest. `reset` clears the review. When the session has an
archived transcript the entry is **kept** and set back to `unreviewed` rather
than deleted: that entry is the only route to the archive, so removing it would
strand the file — unreachable by `harvest show`, `ledger export` and
`ledger list` alike, while still counting against the archive budget. With no
archive behind it the entry is removed outright, and the session is offered
again only while Claude Code still holds the live transcript; the success
message says which of the two happened. To delete an entry *and* its archive,
use `harvest ledger rm`. The ledger lives in
`.engramdb/state/harvest_ledger.jsonl` under the root project, shared by
all its worktrees and by any sub-project linked to it with
`engramdb projects link` — the same root the archives are keyed by, since
those projects are offered each other's sessions and prune each other's
archives. A ledger found at a sub-project's own path (written before it was
linked) is appended to the root's the next time a harvest command runs there,
and the old file is kept alongside as `harvest_ledger.jsonl.adopted`. Moving
it aside is what commits the adoption, so a sub-project directory that cannot
be written to adopts nothing (with a warning naming the path) rather than
re-appending the same entries on every command — which would undo a
`harvest reset` each time.

**The ledger is an append-only log.** Every state change is one JSON line, and
a line records only the fields that change — so the SessionEnd hook writing
where a transcript ended up cannot disturb a decision you recorded, and vice
versa, without either side reading the file first. Reading folds the lines
together in timestamp order, last write wins. A partial line left by a crash
costs that line and nothing else. The file is also only ever read and written
as a **plain file**: if `harvest_ledger.jsonl` is a symlink, a named pipe or
anything else — which a repository you cloned can arrange, since `.engramdb/`
is meant to be committed — every read folds as empty and every write is
refused, each with a warning naming the path. Nothing is deleted for you;
remove the planted path and the ledger works again. The file is rewritten from scratch once it
holds more than **four** lines per live entry, which is what drops entries that
have been removed or aged out.

Upgrading from an earlier version converts `harvested_sessions.json` the first
time any harvest command runs, and keeps the original next to it as
`harvested_sessions.json.migrated`. No review decision is lost; a record the
converter cannot read is skipped with a warning naming the session, and the
original file is your copy of it.

Entries are dropped once they are **365 days** old — a fixed window, with no
config knob — but only ones that hold no archive; an entry naming an archived
transcript is exempt however old it is, because it is the only route to that
file. So a `skipped` session with no archive behind it is eventually forgotten,
and is offered once more if Claude Code somehow still holds the live
transcript. Archiving (on by default) is what makes a review permanent.

Each entry carries two independent fields. A **decision** — what you concluded:
`harvested` (memories saved), `skipped` (reviewed and passed over), `deferred`
(a human looked at it and postponed the call — `--defer` records this), or
`unreviewed` (nobody has looked at it yet). The last is written for every
session the SessionEnd hook collects, so it is by far the most common; keeping
it out of `deferred` is what makes a real deferral findable. Neither settles a
session, so both keep appearing in `harvest list`.

And a **stage** — where the conversation's bytes are: `collected` (a transcript
is reachable), `indexed` (a search row exists for it), or `compressed`. The two
never move each other: a session can be `indexed` while `skipped`, or
`compressed` while `deferred`. `harvest ledger list --stage <stage>` filters on
it, alongside `--decision`. An entry that reaches `compressed` is dropped from
the ledger on the next read, on the premise that something else carries it from
then on. Conversation search now ships, but its row is not that something: it
holds the session's first prompt, curated summary and vectors — not the
decision, the memory ids or the note. Dropping an entry therefore still destroys
its review record, so **nothing in this version writes `compressed`**; the stage
exists only so the format does not have to change later. If something else ever
writes it, the drain says so loudly in the log, naming every session it dropped.

**Transcript archives.** With `[harvest] archive = true` (the default), the
SessionEnd hook compresses each ending session's transcript so it can still
be read later. `harvest show <id>` falls back to the archive automatically
once the live transcript is gone — but a pruned session no longer appears in
`harvest list`, which reads live transcripts only, so find it with
`harvest ledger list` first. This matters because Claude Code prunes its own
transcripts: archiving at *harvest* time would protect nothing, since you
necessarily still hold the file then.

Archives live at `<global_data_dir>/projects/<root_id>/transcripts/` —
deliberately **not** under `.engramdb/`, which is repo-adjacent and gets
committed. Transcripts routinely contain environment variables echoed by
commands and keys pasted into chat; committing them to a shared repository
would be a serious leak.

Real transcripts compress about 4.5x (less than one might assume — most of a
transcript is high-entropy tool output), so a typical session lands around
650 KB. `archive_retention_days` (365) and `archive_max_bytes` (2 GiB, with
oldest-first eviction) bound the total; `harvest ledger prune` reclaims space
on demand and, like `gc` and `compress`, is a dry run until `--apply`. These
budgets are what expire an archive: the ledger's own entry-retention window
never does, because an entry is the only route to the file it names, so a
session whose archive is still held keeps its record however old it is. That
exemption is checked against the archive directory rather than against what the
entry claims — an entry naming a file that `projects delete --cascade`, a
restored backup, or an eviction on another machine took away loses the
reference, and with it the exemption, on the next harvest command.
`harvest ledger rm` deletes one — it confirms first, since once Claude Code has pruned its own transcript the archive is the only remaining copy; `--force` skips the prompt and is **required** under `--format json`, which never prompts. `harvest ledger export` restores one, verifying it against the SHA-256
recorded when it was written.

A conversation lives in **three** places, and `harvest ledger rm <id>` (without
`--archive-only`) removes all three: the ledger entry, the archived transcript,
and the conversation **search row** — which stores the session's first prompt
and its curated summary verbatim, so leaving it would keep the conversation
findable by `harvest search` after you were told the only copy was gone, with
nothing left for `harvest show` to open. `--archive-only` deliberately keeps the
search row, because it retracts nothing: it reclaims bytes while the review
record stands, and dropping the row would destroy a curated summary that no
rebuild can recreate. A session that is searchable but no longer readable is an
ordinary state either way — it is what any indexed session becomes once Claude
Code prunes its transcript and no copy was taken.

**Provenance and pinning.** `harvest mark <session> --memory <id>` records the
session on each named memory as the conversation it was extracted from, so a
memory that is later challenged resolves back to what was actually said
(`harvest show <session>`). The agent does nothing extra for this — `mark` is
already the one call that names both halves. The link is stored in the memory
file itself, so it is committed and travels with a clone; `engramdb get <id>
--format json` shows it as `source_sessions`.

A cited conversation's transcript copy is **pinned**: neither
`archive_retention_days` nor `archive_max_bytes` evicts it, and neither does
the unattended SessionEnd sweep. The budget is measured over the *unpinned*
copies only — counting pinned bytes toward the cap would quietly evict every
unpinned copy to make room for files it is not allowed to touch. Pinned bytes
beyond the budget are therefore reported rather than enforced, by
`harvest ledger prune` and by `doctor`.

Releasing a pin is deliberate: `harvest ledger rm <id>` refuses to delete a
cited copy and names the memories citing it. `--unpin` is the decision to
delete it anyway; `--force` only skips the prompt, so a scripted cleanup can
never strand a memory's evidence by accident. Once the copy is gone the memory
keeps its citation and `doctor` reports it as **evidence expired** — the same
thing that happens naturally when a copy reaches the end of its retention
window. Nothing is broken and nothing needs repairing; the claim still holds,
it just can no longer be traced back.

`doctor` reports up to four harvest facts under **Project → Harvest**, none of
which affects the exit code: sessions due for indexing and ledger lines against
live entries (compaction is opportunistic, so a long log is normal) are always
shown; expired evidence and pinned bytes appear only when there is any. The
whole subsection is omitted for a project that has never harvested.

## `completions` — shell completions

```bash
engramdb completions <bash|zsh|fish|powershell|elvish>
```

Emits the completion script on stdout.
