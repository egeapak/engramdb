# The dashboard

`engramdb dashboard` serves a local web page that shows every EngramDB store
on this machine: your projects, their worktrees and linked sub-projects, the
group stores and the global store. It is a viewer. Nothing you do there
changes a store, and looking at a memory there does not count as using it.

```bash
cd ~/code/my-project
engramdb dashboard
# EngramDB dashboard: http://127.0.0.1:7341/?token=3f9c…
# Current project: /home/me/code/my-project (a1b2c3d4e5f60718)
# Read-only view. Press Ctrl-C to stop.
```

Your browser opens the printed URL (pass `--no-open` to skip that). The
project of the directory you started in is selected; switch with the menu in
the header. Flags are in the [CLI reference](./cli-reference.md#dashboard--local-web-dashboard).

## Views

- **Overview** — memory count, reads in the last 7 days against the 7 before,
  how many memories have been read at all, and how many are challenged. A
  30-day chart of reads (up) and writes (down). A *needs attention* list:
  challenged memories (shown as a red "service disruption"), memories that
  need review, and memories nobody has read in 30 days.
- **Memories** — every memory in a sortable table, filterable by text, kind,
  status and line. *Retrieved*, *injected* and *fetched* are separate
  columns and are never added up: a memory retrieved by a query, put into an
  agent's context by a hook, and opened by id are three different things
  (see [`stats`](./cli-reference.md#stats--store-statistics)). Invalidated
  memories are hidden unless you pick the *Invalidated* or *Everything*
  status.
- **Memory** — the body (a small, safe subset of Markdown), usage counts and
  a 30-day chart, supersedes links, challenges with their evidence, all
  metadata, and a timeline.
- **Health** — the index-against-files part of `engramdb doctor` for the
  selected store: stale rows, orphaned files, content drift, unreadable
  files. The dashboard never repairs; run `engramdb doctor --fix`.
- **Projects** — every store on the machine. A project whose directory is
  gone, or a worktree (which keeps its memories in the main checkout), is
  listed as unavailable with the reason rather than hidden.
- **Route map** — coming with typed edges between memories.

The look follows a transit map: each top-level logical scope (`storage` in
`storage.paths`) is a *line* with its own colour and two-letter badge, and
each memory is a *station* whose shape follows its kind. The header button
switches between the light and dark map; by default it follows your system.

## What it reads, and what it never does

The dashboard opens stores **read-only**. Unlike every other command it does
not create the store's `.gitignore`, build a missing index, or run a schema
migration. A store whose schema is behind this binary is shown with a note;
run any `engramdb` command in that project to upgrade it. A store that was
never indexed on this machine (a fresh clone) is listed as unavailable.

It records no usage either, so browsing does not inflate the counts that
decide which memories get promoted. Counts include events still waiting in a
store's log, and they are as fresh as the log: hooks served by the daemon
show up within a minute.

Semantic search is not in the dashboard yet; the Memories filter is a plain
text match over summaries, ids, tags and scopes.

## Security

The dashboard is a web server on your machine, so it is locked down:

- It listens on `127.0.0.1` only, unless you pass `--bind`. Any other
  address prints a warning: the page is then reachable from the network,
  unencrypted, and only the token protects it.
- Each run makes a new random token. The URL carries it once; the page then
  swaps it for a cookie that scripts cannot read and that the browser never
  sends from another site, and removes it from the address bar and history.
  Without the cookie or the token every request is refused. Another user on
  the same machine can reach the port but not your token.
- It only answers requests addressed to `localhost`, `127.0.0.1` or `[::1]`
  (or the `--bind` address) on its own port. That stops *DNS rebinding*,
  where a web page you visit points its own domain at your machine.
- Anything other than a plain read must come from the dashboard's own page
  and carry a per-run check value, so another site cannot make your browser
  act on it.
- Pages forbid inline scripts, framing and outside resources, and memory
  text is always shown as text: a memory that contains HTML cannot run code
  in the page.

Stop it with Ctrl-C. The token dies with the process.

## Developing the frontend

The page is plain HTML, CSS and JavaScript modules in
`crates/engram-dashboard/web/`, built into the binary. To edit it without
rebuilding:

```bash
engramdb dashboard --assets-dir crates/engram-dashboard/web
```

Files are then read from disk on every request with caching off, so a reload
shows the change.
