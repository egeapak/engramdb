# Projects, the Global Store, and Worktrees

A project is any directory containing `.engramdb/`, identified by a deterministic 16-char hex ID (SHA-256 of the canonical path).

## What `engramdb init` creates

For project `xyz` (a hypothetical 16-char ID):

```
<project>/.engramdb/
  manifest.toml             # project name, embedding fingerprint
  config.toml               # optional overrides
  memories/                 # TOML-frontmatter markdown, one file per memory

<global_data_dir>/projects/xyz/
  lancedb/                  # vector index (metadata + embeddings)
  personal/memories/        # personal-visibility memories (not in project tree)
```

The registry lives at `<global_data_dir>/registry.toml`.

## Project IDs

Every operation that targets a non-current project takes a `project` parameter. It accepts:

- an **absolute path** (`/home/me/code/myproject`),
- the **16-char hex ID** from `engramdb projects list`,
- the literal string `"global"` (cross-project store; see below).

Find a project's ID:

```bash
engramdb projects list
engramdb projects info       # current project
```

## Personal vs shared visibility

When you add a memory, `--visibility` decides where it goes:

- `shared` (default) — `<project>/.engramdb/memories/<id>.md`. Lives in the project tree and is presumed committed to git.
- `personal` — `<global_data_dir>/projects/<id>/personal/memories/<id>.md`. Lives outside the project, isn't visible to other contributors.

A single project has both. They're queried together by default. `personal` is what you want for "things only I care about, not the team."

## The global store

The global store is a project-like store with a fixed, well-known ID that starts with underscores (so it can't collide with a real SHA-256 ID). It's for memories that aren't tied to any particular project: workflow preferences, debugging tricks, cross-cutting hazards.

```bash
# Write to / read from the global store explicitly
engramdb add --global --type preference --title "Always check linter before commit" "..."
engramdb query --global --mode rank --path src/foo.rs

# Or use the MCP project="global" parameter (see agents/mcp-tools.md)

# Include global hits in a regular project query
engramdb query --mode rank --path src/foo.rs --include-global
```

The global data directory is `<global_data_dir>/projects/<global_id>/` (no `.engramdb/` in any project tree — global memories live entirely in user-space).

## Project identity drift

A project's ID is derived from its git remote when it has one, and from its
absolute path when it doesn't. So running `engramdb init` **before**
`git remote add origin …` — the ordinary order when you create a repo locally
and push it later — permanently re-keys the project. The registry keeps the old
ID; every live operation uses the new one, and the symptoms are all silent:

- memories vanish from `list`/`query`, because the live ID's index is empty —
  the `.md` files in `.engramdb/memories/` are untouched;
- **personal** memories become invisible: they live only under the old ID's
  data directory;
- group subscriptions detach — they are recorded against the old ID;
- worktree sub-projects still point at the old ID as their parent.

`engramdb doctor` reports it as a `Project identity` warning, and
`projects discover` reports it as `stale_project_id`. Fix it with:

```bash
engramdb projects repair
```

Do **not** run `engramdb init` to "re-register" — that adds a *second* registry
entry for the same path, with no subscriptions and no parent link. `repair`
migrates the existing entry instead, so both survive, and *copies* the personal
memories across. The old data directory is left in place — a sibling clone of
the same remote may share it, and the registry cannot see one.

## Registry, prune, link, unlink

The global registry tracks every project you've init'd. It supports parent-child relationships and cleanup.

```bash
engramdb projects list                                 # full registry as a tree
engramdb projects list --group none                    # flat, one full path per line
engramdb projects info                                 # current project
engramdb projects stats                                # aggregate stats
engramdb projects delete <id> [-f] [--cascade] [--purge] # deregister; --purge also deletes personal memories + transcripts
engramdb projects link <child_id> --parent <parent_id> # link as sub-project
engramdb projects unlink <child_id>                    # promote back to root
engramdb projects prune [-f]                           # remove stale registry entries + orphan data dirs
engramdb projects discover [PATH] [-y] [--dry-run]     # adopt projects on disk that aren't registered
engramdb projects repair [-f]                          # re-key a project whose ID drifted
```

`projects discover` is prune's mirror image. Prune removes registry entries with
no project behind them; discover finds projects with no registry entry in front
of them. The registry is machine-local and only written when a project is
`init`'d or opened *here*, so a repo cloned with its `.engramdb/memories/`
already committed, a restored backup, or a lost `registry.json` all leave real
projects invisible to `projects list` and every cross-project surface.

```bash
engramdb projects discover ~/src --dry-run   # what's out there?
engramdb projects discover ~/src             # ask per project, then register + index
```

It walks from `PATH` (default: the project directory the invocation resolved
to — inside a linked worktree that is the main checkout) up to
`--max-depth` levels (6), skipping dependency and build trees (`node_modules`,
`target`, `.git`, `vendor`, …) and dot-directories unless `--hidden` is passed.
Each unregistered project is offered individually — accepting registers it and
rebuilds its index from the on-disk `.md` files, with a progress bar over the
batch. `--yes` takes them all, `--no-index` registers without rebuilding (run
`engramdb reindex` later), and `--dry-run` only reports.

Four things are deliberately never auto-registered. engramdb's own global and
group stores (they live under the global data dir in the same `.engramdb/`
layout) are not reported at all. The other three are reported and skipped — as
a warning in human output, and in `skipped[]` in JSON:

- A directory whose project ID is already claimed by a different checkout that
  still exists (in the registry, or earlier in the same scan). Two clones of
  one git remote hash to the same ID and would share a single index.
- A path registered under an ID it no longer hashes to (see [Project identity
  drift](#project-identity-drift) above) — adopting would leave two entries for
  one path.
- A **linked git worktree** carrying its own `.engramdb/`. Worktrees are
  sub-projects: memory operations inside one already route to the main
  checkout, and any stray local store is consolidated into it on the next
  command. Adopting one as a root project would give the same memory files two
  owners, double-counting them in `projects list` and `projects stats`. Run
  `discover` against the main checkout instead.

`projects list` prints a directory tree. Projects are grouped under their
containing folder and sorted by path; a worktree (or any linked sub-project)
nests under its real parent with a `↳` marker:

```
/Users/you/Projects/ceiba
  d66b6ed0c9bfc  audivi (ok)
  3e7b6e498d687  gatekeeper (ok)
    ↳ ae0cb5f27789a  gatekeeper-fda-gaps (ok)   # a linked worktree of gatekeeper
```

The grouping is configurable via `[cli].project_list_grouping` (`auto`
default / `always` / `none`) and can be overridden per run with `--group`
(see [configuration.md](./configuration.md#notes-on-selected-sections)). The
`(ok)` / `(missing)` marker reports whether the project still exists on disk.
For scripting, `engramdb projects list --json` emits a flat array where each
entry carries `parent_project_id`, regardless of the grouping mode.

`delete` refuses by default if the project has children — you must either unlink them first or pass `--cascade` to delete the whole subtree.

`prune` cleans two things:
- **Stale** entries: registered projects whose path no longer exists on disk.
- **Orphan** data: data directories under `<global_data_dir>/projects/` that
  nothing answers to. "Nothing answers to" is wider than "no registry row names
  it": a registered path's *live* ID is protected even when the row records an
  older one (see [drift](#project-identity-drift)), and so are engramdb's own
  global and group stores.

Neither ever deletes data that exists nowhere else. A data directory still
holding `personal/memories/*.md`, `transcripts/*.jsonl.zst` or a
`lancedb/conversations.lance` table is kept whole, index included, and the kept directories
are reported as `retained_irreplaceable`. The reason is structural: a project ID derived from a
git remote is shared by every clone of that remote on this machine, and the
registry records only one of them, so no check can prove a directory is yours
alone. `projects delete --purge` is the one way to say you mean it anyway.

## Git worktrees

When you run a memory operation inside a linked git worktree, EngramDB **routes the operation to the main worktree's project**:

1. Detects the linked worktree via the `.git` file pointing to `<main>/.git/worktrees/<name>`.
2. Ensures the main worktree's project is registered.
3. Registers the current worktree as a sub-project (parent = main).
4. Consolidates any memories previously written to a stray worktree-local `.engramdb/` into the main store.

`add`, `query`, `update`, etc. target one consistent store regardless of which worktree you're in.

**Exceptions.** A few commands deliberately do **not** route to the main worktree:

| Command | Why |
|---------|-----|
| `init` | You may genuinely want a fresh, independent store. |
| `serve` | The MCP server owns its own working dir and target resolution. |
| `completions` | No memory store involved. |
| `setup` | Writes per-directory `.claude/` config; routing would silently target the wrong dir. |
| `daemon` | Process-wide model host, ignores `--dir` entirely. |

To make a worktree a standalone project: `engramdb projects unlink <worktree_id>` after init.

**Session transcripts follow the same rule.** Claude Code files transcripts
under the session's working directory, so each worktree's conversations land
in their own place on disk — but their *memories* route to the main store, as
above. `engramdb harvest` therefore walks the hierarchy from the root and
covers the main checkout plus every registered sub-project in one pass, so
`/engram:harvest` sees the conversations held in your worktrees. The harvest
ledger lives under the root project too, shared by all of them.

## Multi-project workflows

```bash
# Stats across every registered project
engramdb stats --all-projects

# Query my current project plus the global store
engramdb query --mode rank --path src/foo.rs --include-global

# Query a different project explicitly (CLI: use --dir)
engramdb query --dir ~/code/other-project --mode rank --path src/bar.rs

# From an agent (MCP): pass project="<id-or-path-or-global>" on any tool call
```

## Notes

- **`--global` vs `--include-global`.** `--global` operates against the global store **instead of** the current project. `--include-global` operates against the current project **plus** the global store.
- **Project IDs are path-stable** when there is no git remote (with one, the remote decides). Moving such a project produces a new ID — run `engramdb projects prune` after to reclaim the old data directory. Prune keeps it if it holds personal memories, archived transcripts or conversation summaries (which is the common case for any project that has ended a Claude Code session); move what you need across, then `projects delete --purge` the old ID.
