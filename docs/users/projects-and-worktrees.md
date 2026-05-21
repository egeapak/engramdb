# Projects, the Global Store, and Worktrees

EngramDB is project-scoped by default — every memory belongs to a specific project, identified by a 16-character hex ID derived from the project's canonical path. This page explains how that maps to disk, how the global cross-project store works, and how engramdb handles git worktrees transparently.

## What a "project" is

A project is any directory containing `<dir>/.engramdb/`. It's created by:

```bash
engramdb init
```

This:
- creates `.engramdb/` with `manifest.toml`, `memories/`, and an empty `config.toml`,
- computes a deterministic project ID (SHA-256 over the canonical path, first 16 hex chars),
- registers the project in the global registry (`<global_data_dir>/registry.toml`),
- creates the LanceDB vector index under `<global_data_dir>/projects/<id>/lancedb/`.

The on-disk layout for project `xyz` (hypothetical 16-char ID):

```
<project>/.engramdb/
  manifest.toml             # project name, embedding fingerprint
  config.toml               # optional overrides
  memories/                 # TOML-frontmatter markdown, one file per memory

<global_data_dir>/projects/xyz/
  lancedb/                  # vector index (metadata + embeddings)
  personal/memories/        # personal-visibility memories (not in project tree)
```

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

## Registry, prune, link, unlink

The global registry tracks every project you've init'd. It supports parent-child relationships and cleanup.

```bash
engramdb projects list                                 # full registry with hierarchy
engramdb projects info                                 # current project
engramdb projects stats                                # aggregate stats
engramdb projects delete <id> [-f] [--cascade]         # remove from registry + delete data
engramdb projects link <child_id> --parent <parent_id> # link as sub-project
engramdb projects unlink <child_id>                    # promote back to root
engramdb projects prune [-f]                           # remove stale registry entries + orphan data
```

`delete` refuses by default if the project has children — you must either unlink them first or pass `--cascade` to delete the whole subtree.

`prune` cleans two things:
- **Stale** entries: registered projects whose path no longer exists on disk.
- **Orphan** data: data directories under `<global_data_dir>/projects/` that no registry entry points to.

## Git worktrees (the magic)

Linked git worktrees (created with `git worktree add`) share a `.git` but live at separate paths. EngramDB handles this transparently: when you run a memory operation inside a non-main worktree, it **routes the operation to the main worktree's project**.

Specifically:

1. The CLI detects you're in a linked worktree via the `.git` file pointing to `<main>/.git/worktrees/<name>`.
2. It ensures the main worktree's project is registered (initializing it if needed).
3. It registers the current worktree as a **sub-project** (parent = main).
4. If you've previously written memories to the worktree's own stray `.engramdb/` (a common pre-fix mistake), it consolidates them into the main project's store.
5. Then it runs the operation against the main project.

This means `engramdb add`, `query`, `update`, etc. all "just work" — they target one consistent store no matter which worktree you're sitting in.

**Exceptions.** A few commands deliberately do **not** route to the main worktree:

| Command | Why |
|---------|-----|
| `init` | You may genuinely want a fresh, independent store. |
| `serve` | The MCP server owns its own working dir and target resolution. |
| `completions` | No memory store involved. |
| `setup` | Writes per-directory `.claude/` config; routing would silently target the wrong dir. |
| `daemon` | Process-wide model host, ignores `--dir` entirely. |

If you actually want the worktree to be a separate project, run `engramdb projects unlink <worktree_id>` after init. Then it becomes a root project on its own.

## Multi-project workflows

`--all-projects` / `--include-global` and the MCP `project` parameter let you operate across projects:

```bash
# Stats across every registered project
engramdb stats --all-projects

# Query my current project plus the global store
engramdb query --mode rank --path src/foo.rs --include-global

# Query a different project explicitly (CLI: use --dir)
engramdb query --dir ~/code/other-project --mode rank --path src/bar.rs

# From an agent (MCP): pass project="<id-or-path-or-global>" on any tool call
```

## Tips

- **Use stable, canonical paths.** The project ID is derived from the canonical path. Symlinks resolve before hashing, but moving the project directory will produce a new ID (run `engramdb projects prune` afterwards to clean up the orphan).
- **Worktrees should share a project.** Auto-routing makes this the default — don't fight it unless you have a specific reason.
- **`--global` and `--include-global` are different.** `--global` operates against the global store **instead of** the current project. `--include-global` operates against the current project **plus** the global store.
- **Personal memories don't follow the project on git push.** They live in your global data dir. Don't store team-critical info as personal — it dies with your laptop.
