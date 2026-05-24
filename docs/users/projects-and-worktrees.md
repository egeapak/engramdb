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
- **Project IDs are path-stable.** Moving the project directory produces a new ID — run `engramdb projects prune` after to clean up the orphan.
