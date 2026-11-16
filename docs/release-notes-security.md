# Release notes — 0.10.0: security settings and hardening

> Part of [0.10.0](./release-notes-0.10.0.md). This release moves
> `[security]` to the global config and hardens what a cloned repository can
> make EngramDB do.
>
> This page is part of a larger set of changes. Every user-visible change in
> that set, these included, is listed in
> [release-notes-review-fixes.md](./release-notes-review-fixes.md).

## Breaking: `[security]` is read only from the global config file

A project's `.engramdb/config.toml` is committed with the repository. A cloned
repository could set `[security]` there and grant its own agent access to
other projects. `[security]` is now read only from the **global config file**,
like `[daemon]`:

| Platform | Global config file |
|----------|--------------------|
| Linux | `~/.config/engramdb/config.toml` (or `$XDG_CONFIG_HOME/engramdb/config.toml`) |
| macOS | `~/Library/Application Support/engramdb/config.toml` |
| Any | `$ENGRAMDB_CONFIG_DIR/config.toml` when `ENGRAMDB_CONFIG_DIR` is set |

A project config that still has `[security]` loads, but its keys are ignored.
EngramDB warns once per project config and `engramdb doctor` reports it. A key
set to `true` there grants nothing.

## Breaking: cross-project MCP writes are off by default

`allow_cross_project_writes` now defaults to `false`. An MCP tool called with
`project` set to another registered project (`create`, `update`, `delete`,
`challenge`, `resolve`, `verify`, `task_complete`, `compress_apply`, `gc`,
`reindex`, `harvest_mark`, `doctor` with `fix`) is refused until you set:

```toml
[security]
allow_cross_project_writes = true
```

in the global file. The session's own project, its worktrees, group stores and
the global store are not affected. The CLI is not affected.

## Breaking: MCP `projects_link` / `projects_unlink` are gated

Linking changes which past conversations a project's harvest can read. From
MCP, a link that touches a project outside the session's own hierarchy now
needs both `allow_cross_project_writes = true` and
`allow_all_projects_harvest = true` in the global file; an unlink outside it
needs `allow_cross_project_writes = true`. `engramdb setup` no longer
auto-approves these two tools and removes an old approval. `engramdb projects
link` on the CLI is unchanged.

## Breaking: `[nli].model` must be a built-in model

`[nli].model` now accepts only `Xenova/nli-deberta-v3-xsmall` (the default) and
`cross-encoder/nli-deberta-v3-xsmall`. Any other repository is refused and NLI
stays off for that project, unless the global file sets
`[security] allow_custom_model_repos = true`. Restart the daemon
(`engramdb daemon restart`) after changing it.

## Other changes

- **Model files are checked.** The built-in model files EngramDB downloads
  itself (default embedding, T5, NLI, reranker) are checked against a SHA-256
  list. A file that does not match is not loaded, and `doctor` names it.
- **Stored text is marked and escaped on MCP.** `query`, `get`, `list`,
  `review`, `challenge` and `compress_candidates` start with a `trust` field.
  Harness-style tags in memory text (`<system-reminder>` and the like) come
  back escaped as `<\system-reminder>`, in those tools, the `memory://`
  resources and the session-start prompt. Code such as `Vec<String>` is left
  alone. Harvest digests escape every hyphenated tag, not only four names, so
  sessions that contain one re-embed once on the next index pass.
- **The daemon client checks who it talks to.** A daemon socket or pipe
  served by another user is refused; hooks and models then run in-process, and
  `doctor` reports it.
- **ONNX Runtime search order.** The probe never asks the loader to search for
  a bare library name (which on Windows and macOS includes the current
  directory). A relative `ORT_DYLIB_PATH` must exist under the executable's
  directory. See installation.md for the full order.
- **`add --editor`** writes its draft to a private temp file (mode 0600) and
  deletes it on every exit path.
- A spawned daemon starts in its own folder, not in the directory of the
  session that spawned it.
