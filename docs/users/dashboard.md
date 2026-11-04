# The dashboard

`engramdb dashboard` serves a local web page that shows every EngramDB store
on this machine: your projects, their worktrees and linked sub-projects, the
group stores and the global store. It is a viewer: by default nothing you do
there changes a store, and looking at a memory there never counts as using it.
The one exception is opt-in — started with `--allow-writes`, it lets you
confirm or reject *proposed* edges between memories (see
[Reviewing proposed edges](#reviewing-proposed-edges)).

```bash
cd ~/code/my-project
engramdb dashboard
# EngramDB dashboard at http://127.0.0.1:7341/ — opened in your browser
# Current project: /home/me/code/my-project (a1b2c3d4e5f60718)
# Read-only view. Press Ctrl-C to stop.
```

Your browser opens the dashboard, signed in; the terminal shows only the
bare address, which has no token and does not sign you in. To get the
sign-in URL, pass `--no-open`: nothing is launched and the terminal prints
it instead (`EngramDB dashboard: http://127.0.0.1:7341/#token=3f9c…`). If
the browser cannot be launched at all, the sign-in URL is printed as a
fallback, so you are never left without a way in. The
project of the directory you started in is selected; switch with the menu in
the header. Flags are in the [CLI reference](./cli-reference.md#dashboard--local-web-dashboard).

## Views

- **Overview** — memory count; reads in the last 7 days as three numbers
  (retrieved, injected, fetched), each against the 7 days before; how many
  memories have been read at all; and how many are challenged. A 30-day
  chart with one row per signal — retrieved, injected, fetched and writes —
  each on its own scale. A *needs attention* list: proposed contradictions
  first (under a red banner), then challenged memories, memories
  that need review, other proposed edges, and memories nobody has read in 30
  days. Each proposed edge opens a *Review* panel in place.
- **Memories** — every memory in a sortable table, filterable by text, kind,
  status and scope. *Retrieved*, *injected* and *fetched* are separate
  columns and are never added up: a memory retrieved by a query, put into an
  agent's context by a hook, and opened by id are three different things
  (see [`stats`](./cli-reference.md#stats--store-statistics)). Invalidated
  memories are hidden unless you pick the *Invalidated* or *Everything*
  status.
- **Memory** — the body (a small, safe subset of Markdown), usage counts and
  a 30-day chart, a *Review* section for its proposed edges, every edge in and
  out (kind, status, origin, confidence, note, and whether the far end is
  deleted, invalidated or in a store that cannot be checked here),
  challenges with their evidence, all metadata, and a timeline. Its counts are exact; the Memories table reads a
  cheaper summary that can trail them briefly until the next refresh.
- **Health** — the index-against-files part of `engramdb doctor` for the
  selected store: stale rows, orphaned files, content drift, unreadable
  files. The dashboard never repairs; run `engramdb doctor --fix`.
- **Projects** — every store on the machine. A project whose directory is
  gone, or a worktree (which keeps its memories in the main checkout), is
  listed as unavailable with the reason rather than hidden.
- **Graph** — the store's memories and the relations between them
  (below).

Each top-level logical scope (`storage` in `storage.paths`) has its own
colour and two-letter badge, and each memory is drawn with a shape for its
type. The biggest seven scopes get seven colours; further scopes reuse a
colour with a round, then a ringed, badge. A scope keeps its colour on every
page. The header button switches between the light and dark theme; by
default it follows your system.

## The graph

Each memory is a node, coloured by its top scope; memories of one scope are
joined by a path in its colour. Each relation between two memories is an
edge (see [Linking memories](./links.md)):

| Drawn as | Relation |
|---|---|
| Red, with a ⊣ bar at each end | `contradicts` — dashed while proposed, solid once confirmed |
| Heavy line, arrow at the replaced memory | `supersedes` |
| Line with an arrow | `supports` |
| Dotted line with an arrow | `derived_from` |
| Thin grey line / grey dash-dot | `related_to` / `duplicates` |
| Any other kind, faded | proposed, not yet reviewed |
| Ending at a crossed-out circle (⊗) | the far end was deleted (dangling) |
| Ending at a `?` | the far end is in a store that could not be opened here |

Memory size follows reads, so a memory nothing reads is small; a memory with
two or more relations has a heavier outline. Labels are the start of each
memory's summary; where one would overlap another label or a memory it is
left out, and more appear as you zoom in. Filters (kinds, whether to include
proposed, rejected and invalidated, and one scope) live in the address bar,
so a filtered graph can be bookmarked. Toggling a kind, proposed or rejected
only hides or shows relations: memories stay where they are. Drag or use
the arrow keys to pan, a pinch or `+`/`-` to zoom, and `0` or *Fit* to see
it all. The wheel zooms only once you have clicked into the graph (or while
holding Ctrl/⌘), so scrolling the page past it never zooms by accident. `]`
and `[` step through the memories.

Hovering (or focusing) a memory or relation shows a short tooltip.
Clicking it pins it in the details card beside the graph (below it on a
phone, which scrolls to it): a memory's card lists its relations and has an
*Open memory* link; a proposed relation's card is where it is reviewed.
Below the graph, a table lists the proposed relations; *Show all relations*
expands it to every one, for keyboard and screen-reader use.

A large store shows its most connected memories first (800 by default); the
page says when it is partial, and picking a scope shows the rest of it. The
layout runs in a background worker and is deterministic: the same store
draws the same graph.

## Reviewing proposed edges

Proposed edges come from `challenge --against` and from the automatic
proposers (see [Linking memories](./links.md#automatic-proposals)). Start the
dashboard with `--allow-writes` to review them:

```bash
engramdb dashboard --allow-writes
# Writes enabled (--allow-writes): proposed edges can be confirmed or rejected from the page.
```

A proposed `contradicts` edge has four choices:

- **Both stand (conflict)** — the contradiction is confirmed; both memories
  stay valid.
- **This one supersedes the other** / **The other supersedes this one** —
  the edge becomes a `supersedes` edge and the loser's validity window
  closes, exactly like `engramdb link … contradicts … --winner <id>`.
- **Not a contradiction** — the edge is kept as `rejected`, so no proposer
  raises it again.

Any other proposed edge is confirmed or rejected. Every choice asks once more
in the page, saying what will happen, before anything is written. The write
goes through the same code as `engramdb link`, takes the project's write lock,
and records a `linked` event (and `resolved` on a superseded loser) from the
`dashboard` surface. A symmetric edge (`contradicts`, `related_to`,
`duplicates`) is stored on both memories, and a review updates both copies.

Settling a contradiction — any of the four choices — also resolves the NLI
challenge that named the other memory, on either side, and the page says
which memories it cleared. A memory left with no other challenge goes back
to `active`.

Only an edge that is still proposed can be reviewed. Sending the same
review twice (a double click, a second tab) changes nothing and the page says
"Already done"; a different review of an edge someone already settled is
refused with `409 not_proposed`. Without `--allow-writes` the buttons are
shown disabled, with the command to restart. Nothing else on the page writes.

## What it reads, and what it never does

The dashboard opens stores **read-only**. Unlike every other command it does
not create the store's `.gitignore`, build a missing index, or run a schema
migration. (An edge review with `--allow-writes` opens that one store for
writing, like any `engramdb` command, and may therefore do those things.) A store whose schema is behind this binary is shown with a note;
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
- Each run makes a new random token. The URL carries it after `#`, a part
  of the address the browser never sends to any server. The page keeps it
  for this tab only, removes it from the address bar and history, and sends
  it with every request for data; a request without it is refused. There is
  no cookie: cookies are shared by every program listening on your
  machine's `127.0.0.1`, whatever its port. The page's own files (its HTML,
  CSS and scripts) are served without the token, since they hold nothing
  of yours. To open the dashboard in another browser or tab, start it
  with `--no-open` and use the printed sign-in URL.
- The browser is opened through a private file only you can read, so the
  token never appears in a process list. The token reaches your terminal
  only with `--no-open` (or when no browser could be launched, as a
  fallback); otherwise the terminal shows the address without it, so it
  does not end up in terminal scrollback, logs or screen recordings.
- It only answers requests addressed to `localhost`, `127.0.0.1` or `[::1]`
  (or the `--bind` address) on its own port. That stops *DNS rebinding*,
  where a web page you visit points its own domain at your machine.
- Anything other than a plain read must come from the dashboard's own page
  and carry a per-run check value, so another site cannot make your browser
  act on it. The only such requests are the edge reviews, and the server
  refuses those too unless it was started with `--allow-writes`.
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
