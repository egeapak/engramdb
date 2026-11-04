# The dashboard

`engramdb dashboard` serves a local web page that shows every EngramDB store
on this machine: your projects, their worktrees and linked sub-projects, the
group stores and the global store. It is a viewer: by default nothing you do
there changes a store, and looking at a memory there never counts as using it.
The one exception is opt-in — started with `--allow-writes`, it lets you
confirm or reject *proposed* relations between memories (see
[Reviewing proposed relations](#reviewing-proposed-relations)). A
*relation* is what `engramdb link` and [Linking memories](./links.md) call an
*edge*; the dashboard says relation throughout.

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
  that need review, other proposed relations, and memories nobody has read in 30
  days. Each proposed relation opens a *Review* panel in place.
- **Memories** — every memory in a sortable table, filterable by text, type,
  status and top-level scope. *Retrieved*, *injected* and *fetched* are separate
  columns and are never added up: a memory retrieved by a query, put into an
  agent's context by a hook, and opened by id are three different things
  (see [`stats`](./cli-reference.md#stats--store-statistics)). Invalidated
  memories are hidden unless you pick the *Invalidated* or *Everything*
  status.
- **Memory** — the body (a small, safe subset of Markdown), usage counts and
  a 30-day chart, a *Review* section for its proposed relations, every relation in
  and out (kind, status, origin, confidence, note, and whether the far end is
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
- **Relations** — every relation as a scope × scope matrix and a list that
  doubles as the review queue ([below](#the-relations-tab)).

Each top-level logical scope (`storage` in `storage.paths`) has its own
colour and two-letter badge, and each memory is drawn with a shape for its
type. The biggest seven scopes get seven colours; further scopes reuse a
colour with a round, then a ringed, badge. A scope keeps its colour on every
page. The header button switches between the light and dark theme; by
default it follows your system.

## The graph

The *Style* picker in the graph's toolbar changes how the graph is drawn,
never what it shows: filters, selection, the details card and the relations
table work the same in every style. Your choice is remembered in this
browser. Picking a style also writes it into the address bar (`style=`): a
link that carries `style=` opens in that style (without changing the other
person's own choice), and a link without it opens in each person's own
choice. Two styles exist today:

- **Plain** (the default) — memories placed by their relations and gathered
  by scope: related memories sit close together, and each top-level scope is
  a soft tinted region with its name. Memories are all one neutral colour, so
  colour is left to the relations (a memory nothing has read yet is drawn
  hollow); the first seven scopes get seven tints, scopes eight to fourteen
  add a diagonal hatch over the tint, fifteen to twenty-one a dot texture
  and twenty-two to twenty-eight a cross-hatch. Every region is named, which
  is what tells scopes apart past twenty-eight. The same store always draws the
  same graph in a box of the same shape (the window's width changes only how
  large the scope names are drawn). Small graphs are spread out so every
  memory's label has room at Fit on a desktop-sized window; on a phone, or in
  a large graph, zoom in for labels or hover a memory to name it.
- **Grid** — the original look: memories on a grid, each top-level scope a
  coloured path through its memories with a badge at its start, and
  relations with arrowheads. On the graph page with Grid picked, the
  header's light/dark button reads *Night map* / *Day map* and the page is
  titled "*project* network"; every other page keeps *Dark* / *Light*.

The shapes between memories are relations (see [Linking
memories](./links.md)). The legend under the graph shows each one exactly as
the picked style draws it. In **Plain** there are no arrowheads: a relation
with a direction is a **tapered** shape, **wide at the memory that
supports, supersedes or is derived from the other**, and narrow at the
memory it points to. A directed relation too short on screen to show its
taper (under about 40 pixels) also carries a small chevron in the middle
pointing the same way. Kind is told
by colour *and* by shape, so it reads without telling colours apart:

| Plain draws | Relation |
|---|---|
| Red zigzag | `contradicts` (no direction) |
| The heaviest taper, in the text colour (dark on light, light on dark) | `supersedes` — wide at the newer memory |
| Green tapered | `supports` — wide at the supporting memory |
| Blue taper with cross ribs standing out of it, like a comb | `derived_from` — wide at the derived memory |
| Violet double stroke | `duplicates` (no direction) |
| Thin grey | `related_to` (no direction) |
| Dashed | proposed, not yet reviewed; a directed one has one chevron in the middle pointing at the target |
| Faint and dotted | rejected (drawn only with the *Rejected* filter on) |
| Fading out toward its far end | the far end is not on the graph: ending in a short bar (and fading less), it was deleted (dangling); without one, it is a memory in another store, or in a store that could not be opened here |

Relations keep a readable width at any zoom: they thicken a little as you
zoom in, but much less than the memories do, and once a zoom comes to rest
their dashes, ribs and zigzags are redrawn at a size that suits it. In Grid, a contradiction is red
with a ⊣ bar at each end, the directed kinds end in an arrow, a deleted far
end is a crossed-out circle (⊗) and an unreadable one a `?` (a live memory
in another store has no end mark).

Memory size follows reads, so a memory nothing reads is small; a memory with
two or more relations has a heavier outline. Labels are the start of each
memory's summary; where one would overlap another label or a memory it is
left out, and more appear as you zoom in. Filters (kinds, whether to include
proposed, rejected and invalidated, and one top-level scope) live in the address bar,
so a filtered graph can be bookmarked. Toggling a kind, proposed or rejected
only hides or shows relations: memories stay where they are. Drag or use
the arrow keys to pan, a pinch or `+`/`-` to zoom, and `0` or *Fit* to see
it all. The wheel zooms only once you have clicked into the graph (or while
holding Ctrl/⌘), so scrolling the page past it never zooms by accident. `]`
and `[` step through the memories.

### Selecting a memory

Hovering (or focusing) a memory **focuses** it: its own relations are drawn
at full strength, its neighbours (the memories at their other ends) are
labelled (their labels are placed again among themselves so none overlaps;
a neighbour that still finds no room is named in the tooltip instead), and
everything else fades. A short tooltip in the corner of the graph away
from the memory names it. Hovering a relation lights it and its
two ends the same way.

Clicking a memory **selects** it: the fade stays, and the details panel
beside the graph (below it on a phone, which scrolls to it) shows the
memory — summary, type, scope, criticality, status and reads, with an *Open
memory* link — and then its relations **in words**, read from the selected
memory, so direction never depends on reading a shape:

| Group | Means |
|---|---|
| Contradicts | the two disagree (listed first) |
| Supports / Supported by | this memory supports the other / the other supports this one |
| Supersedes / Superseded by | this memory replaces the other / the other replaces this one |
| Derived from / Source of | this memory was derived from the other / the other was derived from this one |
| Duplicates | the two say the same thing |
| Related to | the two are related |
| any other kind | by its name |

A line above the groups sums them up ("contradicts 1 · supported by 2 ·
related to 5"). Each row names the other memory (click it to select that
memory instead), its scope or *same scope*, a **Proposed** badge for a
relation awaiting review and **Rejected** for a rejected one (shown with the
*Rejected* filter on), and a note when the other end is not on the graph:
*dangling* (it was deleted) or *not checked* (in a store this dashboard
could not open). In an opened scope, the relations to other scopes come
last, one row per scope ("dashboard · 2 relations", with ⚠ for
contradictions); clicking one opens that scope. The panel follows the
filters, like the graph.

With `--allow-writes`, each proposed relation is reviewed right in its row
(see [Reviewing proposed relations](#reviewing-proposed-relations)); on a
read-only dashboard the panel shows no review controls, only a note saying
how to enable them. Clicking a relation in the graph (or *Review* in the
table below) still pins that one relation in the panel, with its origin,
confidence and note.

Keys, with the graph focused:

| Key | Does |
|---|---|
| `]` / `[` | with nothing selected, step through the memories; with a memory selected, step through its panel's rows in order, focusing each neighbour in the graph (on a phone, focus stays on the row and the neighbour is ringed); a screen reader hears each row, e.g. "Supported by: …" |
| `Enter` | select the focused memory; on a row for another scope, open it |
| `Escape` | clear the selection and return focus to the graph |
| arrows, `+` / `-`, `0` | pan, zoom, fit |

`]`, `[` and `Escape` also work from inside the panel.

Below the graph, a table lists the proposed relations; *Show all relations*
expands it to every one, for keyboard and screen-reader use.

A large store shows its most connected memories first (800 by default); the
page says when it is partial, and picking a top-level scope shows the rest of it. The
layout runs in a background worker and is deterministic: the same store
draws the same graph.

### Scope overview and opened scopes

A store with **more than 150 memories** (after the *Invalidated* filter)
opens on a **scope overview** instead of every memory, in every style: one
disc per top-level scope, its area following its memories, placed next to
the scopes it shares the most relations with; one grey band per pair of
related scopes, its width following how many relations they share; and a
red **⚠ count** on every pair with a contradiction between them. Bands with
fewer than 2 relations, or under 12% of the largest pair, are left out (the
note above the graph says how many), but a pair's ⚠ is always shown, on a
red dashed hairline when its band is left out. The counts cover the whole
store, not the 800 memories a graph draws. A relation to a deleted memory,
or into another store, belongs to no scope pair, so it is not counted here;
the notes on a full graph count those. The kind, *Proposed* and
*Rejected* filters re-count the bands in place. Below the overview, a table
lists every scope pair, contradictions first.

Click a scope (or Tab to it and press Enter) to **open** it. An opened scope
shows its memories, gathered into one region per sub-scope (`storage.lock`,
`storage.paths`, …), with the relations between them drawn as usual. Each
relation to a memory in *another* scope is a faint line to a tag at the
graph's edge, one tag per other scope, placed in the direction that scope
lies on the overview: "storage · 12 ⚠ 2" means 12 relations to `storage`, 2
of them contradictions. Clicking a tag opens that scope; *All scopes* above
the graph goes back. Picking a scope from the filter row opens it the same
way; in a store of 150 memories or fewer it filters the usual graph to that
scope instead. *Show all memories* on the overview draws the whole graph
(still at most 800 memories), with an *All scopes* button to go back.

On a graph of more than 150 memories zoomed in to three times *Fit* or
closer, only relations with both ends on screen are drawn whole; one with a
single end on screen is a short stub from that end, so long relations
crossing the view do not bury the local ones. A store of 150 memories or
fewer is always drawn whole, as above.

## The Relations tab

The Relations tab answers the questions the graph is bad at: exact counts,
which scopes are coupled, every relation of one kind, and what is waiting
for review — at any store size, on a phone, and with a screen reader.

- **The matrix** counts relations between top-level scopes (*Sub-scopes*
  switches to `storage.lock`-level rows and columns). Rows are the source
  scope, columns the target: a directed relation (`supports`, `supersedes`,
  `derived_from`) counts in its (source, target) cell, a symmetric one
  (`contradicts`, `related_to`, `duplicates`) in both, so a cell's number is
  exactly what clicking it lists. A red dot marks a cell with
  contradictions. The cells are one Tab stop: arrow keys move between them,
  Home/End go to the row's ends, Enter picks a cell (again to clear it); each
  cell is read out in words ("storage to hooks: 12 relations, 2
  contradictions"). The matrix follows the kind, status and text filters but
  not the picked cell, so picking one never empties it. A relation to a
  deleted memory or into another store has no far scope and is counted under
  the matrix instead.
- **The list** shows each relation as a sentence ("*A* supports *B*"), each
  end's scope, its status, origin, confidence and age, links to both
  memories and a *Show in graph* link that opens the graph with the source
  memory selected (in its scope, above 150 memories). Proposed relations come
  first, then contradictions, then the newest; 100 at a time, *Load more*
  for the next 100. Filter by kind, status (proposed and confirmed by
  default; *Rejected* or *Every status* to see rejected ones), text (both
  memories' summaries and the note), and invalidated memories. With
  `--allow-writes`, every proposed relation has the review controls (see
  [below](#reviewing-proposed-relations)); after a review the list reloads
  as many rows as you had loaded and puts the result on the reviewed
  relation's row (or at the top, if it is no longer listed). Supersedes
  relations always point at an invalidated memory, so they are listed only
  with *Invalidated* on.

Every filter and the picked cell are in the address, so a view can be
linked. The graph's relations table links here with its filters (*Open in
Relations*).

The API behind it, `GET /api/projects/{id}/relations`, takes `kind=`
(repeatable or a comma list), `status=` (`proposed`, `confirmed`,
`rejected`, `all`; default every status but rejected, or add
`include_rejected=true`), `q=`, `include_invalidated=`, the scope pair
`from_scope=`/`to_scope=` (exact names, `-` for no scope; a symmetric kind
matches either order), `touches=` (either end in a scope), `subs=1`, `limit=`
(100, at most 500) and `cursor=`. It answers `total`, `items`, `counts`,
`next_cursor` (`null` on the last page) and, on the first page, `matrix`:

```json
{"total": 28, "limit": 100, "next_cursor": null, "memories": 1000,
 "counts": {"proposed": 7, "contradictions": 2},
 "items": [{"id": "…|contradicts|…|", "src": "…", "dst": "…", "kind": "contradicts",
            "symmetric": true, "status": "proposed", "origin": "nli", "confidence": 0.91,
            "at": "2026-11-03T14:05:15Z", "state": "live",
            "src_summary": "…", "dst_summary": "…",
            "from": {"summary": "…", "scope": "storage.paths", "top_scope": "storage",
                     "color_slot": 0, "invalidated": false},
            "to": {"…": "…"}}],
 "matrix": {"level": "top", "relations": 700, "unplaced": 3,
            "scopes": [{"name": "storage", "top": "storage", "slot": 0}],
            "cells": [{"from": "storage", "to": "hooks", "count": 12, "contradictions": 2,
                       "proposed": 4, "kinds": {"contradicts": {"proposed": 2}}}]}}
```

Paging is keyed, not by offset: the cursor is the last row's place in the
order, so relations that stay as they were are never repeated or skipped
when others are reviewed or added between two pages. The relation that
changed is the exception: a reviewed one moves out of *proposed* and can show
up again further down among the confirmed ones, and one added ahead of the
cursor appears only when the list starts over. The page reloads its list
after a review for that reason. A cursor belongs to the filters it was made
with; sent with others it is refused (`400`). `q` is at most 256
characters.

## Reviewing proposed relations

Proposed relations come from `challenge --against` and from the automatic
proposers (see [Linking memories](./links.md#automatic-proposals)). Start the
dashboard with `--allow-writes` to review them:

```bash
engramdb dashboard --allow-writes
# Writes enabled (--allow-writes): proposed relations can be confirmed or rejected from the page.
```

A proposed `contradicts` relation has four choices:

- **Both stand (conflict)** — the contradiction is confirmed; both memories
  stay valid.
- **This one supersedes the other** / **The other supersedes this one** —
  the relation becomes a `supersedes` relation and the loser's validity window
  closes, exactly like `engramdb link … contradicts … --winner <id>`.
- **Not a contradiction** — the relation is kept as `rejected`, so no proposer
  raises it again.

Any other proposed relation is confirmed or rejected. Every choice asks once more
in the page, saying what will happen, before anything is written. The write
goes through the same code as `engramdb link`, takes the project's write lock,
and records a `linked` event (and `resolved` on a superseded loser) from the
`dashboard` surface. A symmetric relation (`contradicts`, `related_to`,
`duplicates`) is stored on both memories, and a review updates both copies.

Settling a contradiction — any of the four choices — also resolves the NLI
challenge that named the other memory, on either side, and the page says
which memories it cleared. A memory left with no other challenge goes back
to `active`.

Only a relation that is still proposed can be reviewed. Sending the same
review twice (a double click, a second tab) changes nothing and the page says
"Already done"; a different review of a relation someone already settled is
refused with `409 not_proposed`. Reviews happen in the selected memory's
panel (each proposed row), in a pinned relation's panel, on the memory
page, and in the Relations tab's list (which, read-only, shows one note
instead of controls). Without `--allow-writes` the memory panel shows no buttons, only a
note, and the other two show them disabled, with the command to restart.
After a review the graph reloads to show the new status, with the same
memory selected and the result read out; if that memory was just
superseded (hidden unless *Invalidated* is on), the other end of the
relation is selected instead, and the note says so. Nothing else on the page writes.

## What it reads, and what it never does

The dashboard opens stores **read-only**. Unlike every other command it does
not create the store's `.gitignore`, build a missing index, or run a schema
migration. (A relation review with `--allow-writes` opens that one store for
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
  act on it. The only such requests are the relation reviews, and the server
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
