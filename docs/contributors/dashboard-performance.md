# Dashboard performance

The graph view's budgets (graph view v1.1, phase H), how they are measured,
and the latest figures. Everything here is a developer tool run by hand; no
CI job runs it, because the numbers depend on the machine.

## Budgets

p50, 4 cores, Chromium, release build.

| what | budget |
|---|---|
| `/graph` response | 1k ≤ 150 ms, 2k ≤ 300, 10k overview (`summary_over`) ≤ 400 |
| layout, cold, in the worker | 1k ≤ 400 ms, 2k ≤ 1000 |
| tab click → first full paint | 1k ≤ 800 ms, 2k ≤ 1500, 10k (overview) ≤ 800 |
| hover / focus frame | ≤ 16 ms |
| pan / zoom | 60 fps (2k ≥ 45) |
| open a scope | ≤ 300 ms (2k / 10k ≤ 500) |
| display-filter toggle | ≤ 50 ms (2k ≤ 100) |
| relations, first page | ≤ 200 ms (10k ≤ 300) |

## Method

Three tools, from the server inward to the pixels:

1. **The stores.** `cargo run --example dashboard_bench_store -- <dir>`
   writes four deterministic projects, `s60`, `s1k`, `s2k`, `s10k`, under
   one machine dir (`<dir>/{data,conf,models}`), in about 10 s once the example is built (debug). The
   generator (`examples/support/dashboard_store.rs`) follows the layout
   harness's (`crates/engram-dashboard/bench/data.mjs`): ten top scopes with
   sub-scopes and Zipf-like sizes, 0.7 relations per memory, mostly inside a
   scope, cross-scope ones on a few affine pairs; every kind; 70 % confirmed,
   20 % proposed, 10 % rejected; `supersedes` through the canonical fields
   (target invalidated); 1 % of relations to a deleted memory; 2 %
   challenged and 3 % invalidated memories; read events drained into the
   usage table. It writes `.md` files and runs one `reindex`: no model, and
   no per-memory LanceDB version.
2. **The handlers.** `cargo bench -p engram-dashboard --bench dashboard_graph`
   (Criterion) builds the same stores in a temp dir and drives the real
   router (security layers included) with `oneshot`, as `src/tests.rs` does:
   `graph_overview` (`summary_over=150`, what the tab asks first),
   `graph_full` (`limit=2000`), `graph_top_scope` (`top_scope=storage`, the
   largest scope) and `relations_first_page` (`limit=100`). The store is
   already in the dashboard's read-only cache, as for a page's second
   request. `DASHBOARD_BENCH_SIZES=s1k,s10k` narrows it.
3. **The page.** `crates/engram-dashboard/bench/browser/run.mjs`
   (Playwright; `bench/browser/README.md`) starts
   `engramdb dashboard --no-open` on each store and reads the page's own
   marks (`graph:data`, `graph:layout`, `graph:built`,
   `graph:overview-built`, `relations:painted`) plus `requestAnimationFrame`
   frame times, for both styles, light and dark, 1280 and 390 px wide.
   "Painted" is the first frame after the mark. Every timed load is a fresh
   browser context (no layout cache); `--assets web` serves the frontend
   from disk, so a JS change is measured without a rebuild. It also posts
   each store's whole graph (up to the server's 2000 cap) to a fresh
   `layout-worker.js` for the cold layout figure. `node bench/layout.mjs`
   (the layout harness) covers layout quality; see `bench/README.md`.

What the page lays out is bounded by the server: past 150 memories the tab
opens on the scope overview, and an opened scope or "show all" asks for at
most 800 memories (`limit`), so the 10k store's opened scope is 800
memories and its show-all graph is 800 of 10,000.

## Machine

Cloud sandbox: 4 × Intel Xeon @ 2.10 GHz, 15 GB, no GPU (Chrome rasterizes
in software, so paint-bound figures are pessimistic against a desktop),
Google Chrome for Testing 153, Node 22, release `engramdb` (fat LTO).

## Results

Medians over the 8 combinations (2 styles × 2 themes × 2 widths, 3 cold
loads each, `--runs 3`; the script's default is 5), with the worst combination in brackets (slowest time, lowest
fps). Before = branch head before phase H's fixes, after = with them, same
binary, same stores.

| p50 (worst) | s60 | s1k | s2k | s10k | budget |
|---|---|---|---|---|---|
| `/graph`, as the page sees it (fetch + parse) | 15 (16) | 16 (19) | 20 (23) | 48 (54) | 150 / 300 / 400 |
| tab click → painted | 94 (113) | 49 (50) | 55 (71) | 105 (123) | 800 / 1500 / 800 |
| layout cold in the worker, scope-force (Plain) | 17 | 111 | 179 | 188 (2000 of 10k) | 400 / 1000 |
| layout cold in the worker, grid | 21 | 197 | 322 | 393 (2000 of 10k) | 400 / 1000 |
| open the largest scope | – | 146 (177) | 176 (210) | 307 (355) | 300 / 500 / 500 |
| show all (≤ 800 memories) | – | 314 (362) | 341 (386) | 346 (394) | (tab click) |
| filter toggle, before → after | 18 (19) → 18 (19) | 22 (24) → 22 (23) | 26 (28) → 24 (24) | **47 (54) → 38 (40)** | 50 / 100 |
| hover frame work, p95 (`hover_frame_ms_p95`) | 3 (4) | 6 (7) | 8 (11) | 12 (14) | 16 |
| hover → painted latency, p95 (`hover_ms_p95`) | 19 (21) | 22 (23) | 24 (26) | 27 (31) | (see below) |
| frame length during the hovers, p99 | 16.8 (16.8) | 16.8 (16.8) | 16.8 (16.8) | 16.8 (16.8) | one 60 Hz frame |
| zoom fps, before → after | 60 (59) → 60 (60) | 60 (58) → 60 (60) | 58 (54) → 60 (60) | **48 (33) → 59 (56)** | 60 (2k ≥ 45) |
| pan fps | 60 (57) | 60 (60) | 60 (60) | 59 (59) | 60 |
| relations, first page | 41 (45) | 63 (70) | 72 (90) | 121 (141) | 200 / 300 |

Rows without "before → after" did not move beyond run-to-run noise (the
fixes touch neither the server, the layouts nor the first build). The
"tab click" at s60 is the whole graph (layout included); from s1k up it is
the overview. The s10k interactive rows (toggle, hover, zoom, pan) are
measured on the opened largest scope, which is 800 memories (the server's
limit) and 487 drawn relations.

**Hover.** The budget is the hover's own frame: `hover_frame_ms` dispatches
the events at the start of a frame and stops at the task after that
frame's paint, so it is the main-thread work (style, layout, paint)
the change costs: at most 14 ms at p95, under 16. `hover_ms` is the
latency a user sees and includes waiting for the next vsync (up to
16.7 ms), so it is always above one frame and is not the budgeted figure.
One earlier full run (`after.json`, before this metric existed) recorded a
single 66.6 ms frame in the s2k Plain light 390 combination (its p99 over
~200 frames); four reruns of that combination gave 16.8, so it is counted
as a one-off (a GC or compositor stall), not as a hover cost.

Server side (Criterion, in process, `--profile bench`):

| handler, mean (response size) | s60 | s1k | s2k | s10k | budget |
|---|---|---|---|---|---|
| `graph_overview` (`summary_over=150`) | 5.4 ms (whole graph, 28 KB) | 6.6 ms (12 KB) | 9.9 ms (17 KB) | 35.7 ms (27 KB) | 10k ≤ 400 |
| `graph_full` (`limit=2000`) | 5.4 ms | 10.8 ms (498 KB) | 17.0 ms (979 KB) | 55.2 ms (2000 of 10k, 1.1 MB) | 1k ≤ 150, 2k ≤ 300 |
| `graph_top_scope` (`storage`) | 5.4 ms | 10.0 ms | 14.5 ms | 49.6 ms (800 memories) | (open scope) |
| `relations_first_page` (100 rows) | 4.2 ms | 7.0 ms | 10.4 ms | 36.7 ms | 200 / 300 |

Every handler is at least 6× under budget; at 10k the cost is reading the
whole index (`store.list` and `graph_of`), which every endpoint does first.

## Fixes made in phase H

- **Zoom restyle.** Plain's relations read their width from one custom
  property on the `<svg>` (`--zw`), written on every wheel step. One write,
  but every relation's transform reads it, so each step restyled all of
  them: 30 fps zooming the 10k store's opened scope (487 relations), 58 with
  the write removed. The core now calls a style's `zoomed` once for the
  first view and once each pan / zoom settles (the 160 ms settle timer that
  already decided redraws); mid-gesture, relations scale with the map and
  take their width back on settle. Plain at 10k: 33.2–40.3 → 56.4–60.0 fps.
- **Filter toggle.** A display-filter chip rebuilt every drawn relation
  (path data, element, three listeners) and the browser restyled them all.
  `drawEdges` now keeps the elements it built (keyed by scale, and the view
  in cut mode) and a toggle only sets `hidden` on the ones it drops, built
  lazily the first time they are shown and kept in index order, so what
  overlaps what never changes. 10k opened scope: 42.5–54.2 → 31.8–40.5 ms.

Grid is pixel-identical before and after (light / dark, 1280 / 390, the
full graph, show-all, an opened scope, after each toggle and after a zoom),
up to the same few anti-aliased pixels two runs of the old code differ by.

Not needed at these sizes, so not built: the sessionStorage layout cache,
better start positions, one path per kind and a canvas renderer for big
opened scopes. Every layout and first-paint figure is under a third of its
budget.

## Still over

- **Zoom on the 10k store's opened scope (800 memories): 56–60 fps.**
  800 memories is fewer than the 2k store's graph, so the 60 fps row
  applies, not the 2k ≥ 45 floor. Two full runs: Grid 53.2–59.2 and
  56.4–60.0 fps, Plain 57.1–59.2 and 56.4–60.0 (before the fix Plain was
  33.2–40.3). Both styles therefore miss 60 by up to 7 fps in the worst
  combination; both clear 45. Frames drop only between Fit and about 3×
  Fit, where every memory is on screen at a larger raster scale; zoomed
  further in, the rate is 60. It is paint-bound in Chrome's software
  rasterizer (the sandbox has no GPU); a GPU-rastered desktop should not see
  it, but that is not measured here.
  **Since fixed** for wheel and pinch: a gesture now moves the drawn map
  as one composited layer (a CSS transform on the `.g-pane` wrapper; on the
  `<svg>` itself the transform still re-laid out and re-painted it) and sets
  the view box once it settles, and hover is ignored mid-gesture (the map
  moving under a still pointer hovered a memory, with its tooltip and
  labels, nearly every frame). At 4× CPU throttle (`--cpu-throttle 4`),
  which makes the difference visible: 23 → 59 fps; unthrottled 59–60. The
  view after a gesture is the same; the box's rounded corners differ by one
  colour level on ≤ 16 pixels after one (Chrome's corner clip once the
  layer existed), the graph itself does not.
- The 10k filter toggle (42.5–54.2 → 31.8–40.5 ms over the two runs) is
  now under 50 in every combination; before, it was over in some.

## Server-side layout

`/graph?layout=scope-overview,scope-force[,opened]&layout_aspect=…&layout_width=…`
answers with a `layout` object computed in Rust (`src/layout/`, a port of
`scope-force.js`, `scope-overview.js`, `labels.js`, `radius.js` and the input
builder `graph/input.js`). The page decodes it and skips its own layout; it
lays out as before (scope-force in the worker, the overview on the main
thread) when the response has none, or the header (`v`, name, node/edge
counts, aspect, width, `groups`, and `check`, a checksum of the input the
page recomputes) does not match its own input, or a decoded number is not
finite. Parity (`node bench/server-layout.mjs`, every key compared with
`Object.is`, so bit for bit, not within a tolerance): **0 differences** for
positions, routes, labels and scope names on every bench store and path
(tab, opened scope, show all) at 1280 and 390 px, the same labels placed
and the same crossings. The same comparison runs in CI on three committed
fixtures (`bench/golden.mjs`, `the_ports_match_the_js_on_the_golden_fixtures`),
and a hash of the layout JS pinned to `LAYOUT_VERSION` fails CI when the JS
changes without the port.

Encoding (`layout.v = 2`): positions as little-endian f64 in base64,
one byte per label (the `spotsAt` index, 255 = unplaced), stub ends only
(straight routes come from positions), scope names and region radii as
JSON. For 800 memories: ~20 KB (14 KB gzipped) inside a ~450–530 KB
response; the worker's full output as JSON would be ~150 KB. f64 was kept
over integer-scaled positions (8.5 KB) because it is exact.

Computation: single-flight per input, at most 2 layouts at once (a request
that finds both busy goes without), waited for at most 250 ms; a late layout
still lands in the cache for the next request. scope-force caches its
width-free core and places the scope names per request, so one entry serves
every box width.

Cache: 32 layouts in memory per process, plus a disk cache (64 MiB, LRU by
mtime) under `<cache>/engramdb/dashboard/layouts` (0700 dir, 0600 files,
`ENGRAMDB_DASHBOARD_CACHE_DIR` overrides, `ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0`
disables both), keyed by SHA-256 of the layout version, the layout JS hash,
the crate version, the name and the input. Never written into a store.

Browser, Plain, light, 1280 px, release, median of 5 runs, ms; *cold* =
no layout cached on the server, *cached* = a repeat. Segments: response,
client layout (worker, or input + key + decode), build parts, paint.

| path | before (worker) | after, cold | after, cached |
|---|---|---|---|
| s60 tab | 72 (resp 11, layout 39) | 42 (15, 5) | 39 (13, 5) |
| s1k open scope | 109 (12, 50) | 74 (26, 4) | 67 (13, 5) |
| s1k show all | 224 (18, 107) | 151 (47, 11) | 124 (20, 11) |
| s2k open scope | 134 (17, 57) | 110 (40, 7) | 90 (18, 7) |
| s2k show all | 239 (23, 108) | 179 (51, 12) | 134 (25, 11) |
| s10k open scope | 241 (57, 88) | 187 (83, 9) | 169 (56, 10) |
| s10k show all | 263 (54, 122) | 198 (86, 25) | 178 (60, 25) |

Then (encoding v2, width-free cache, `graph/input.js`, delegated node and
relation listeners, labels built only when shown, no layout reads during
the build, a cached number formatter, panels not rebuilt on the first draw),
same method, 1280 px, total ms (response): s1k open scope cold 69 (26) /
cached 53 (14); s1k show all 134 (49) / 106 (22); s2k open scope 90 (37) /
73 (22); s2k show all 150 (52) / 118 (26); s10k open scope 173 (93) / 147
(68); s10k show all 174 (90) / 137 (60); s10k tab 77 / 76. s1k show all's
build parts now: nodes 12, view 1, legend 1, edges 13, panels 5, paint 39
(the paint gained the style and layout the build no longer forces). Every
cold run still got a server layout under the 250 ms wait.

The build parts (s1k show all: nodes 21–24, view 9–10, legend 7, edges 15,
panels 6–7, paint ≈ 31) did not change. A tab click on the overview is
unchanged (35–89). Grid is pixel-identical; Plain differs by at most one
colour level on a few anti-aliased pixels.

Server side (Criterion, `dashboard_graph` `*_layout` cases, the whole
`/graph` handler with a layout asked for; cold = cache off, cached = memory
hit): s1k opened scope 21.3 → 9.0 ms, show all 36.5 → 10.1, overview 5.8
either way; s2k opened scope 29.3 → 12.8, show all 39.4 → 14.1, overview
8.7 → 8.1. So the Rust layout itself costs ≈ 12–26 ms per 800 memories,
against 50–122 ms for the worker in the browser.

## Rerun

```bash
cargo run --example dashboard_bench_store -- /tmp/dash-bench
cargo bench -p engram-dashboard --bench dashboard_graph
cargo build --release --bin engramdb
cd crates/engram-dashboard
NODE_PATH=$(npm root -g) node bench/browser/run.mjs --stores /tmp/dash-bench --runs 3 --json /tmp/dash-bench/results.json
```

Close other heavy work first: a parallel `cargo build` doubles every
browser figure. Compare runs on one machine only.
