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
budget. (Plain has since moved to a canvas: see "Canvas surface" below.)

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
counts, aspect, width, `check`, a checksum of the input the page
recomputes — `inputCheck` for scope-force, `overviewCheck` for the overview,
whose band `k` is drawn along route `k`, so pair order is checked too — and
for scope-force `groups`) does not match its own input, or a decoded number
is not finite. Parity (`node bench/server-layout.mjs`, every key compared with
`Object.is`, so bit for bit, not within a tolerance): **0 differences** for
positions, routes, labels and scope names on every bench store and path
(tab, opened scope, show all) at 1280 and 390 px, the same labels placed
and the same crossings. The same comparison runs in CI on three committed
fixtures: `node bench/golden.mjs --check` (the CI *Check & Lint* job) fails
when the JS output no longer matches them, and
`the_ports_match_the_js_on_the_golden_fixtures` when the Rust output does
not. A hash of the layout JS (everything an input is built from: the
layouts, `input.js` with its overview pairing `scopePairs`, and the decoder)
pinned to `LAYOUT_VERSION` fails CI when the JS changes without the port.

Encoding (`layout.v = 3`): positions as little-endian f64 in base64,
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

## Canvas surface (Plain)

Plain now draws the graph on a canvas (`web/js/graph/canvas/host.js`, the
style's hooks in `themes/plain/canvas.js`); Grid, the scope overview and
the legend swatches stay SVG. The view talks to either through one surface
interface (`graph/surface-svg.js` lists it), so nothing in `views/graph.js`
names a style.

**Method.** `bench/browser/views.mjs` (README) holds one view fixed and is
run once with the old `web/` (branch head before the canvas, served with
`--assets` from the same release binary) and once with the new: s1k and
s2k show all, the 10k store's largest scope opened, and its show all (800
of 10,000), Plain, light, 1280 px, medians of 5 fresh contexts (3 for the
DPR 2 and CPU ÷4 GPU rows). "cold" has the server's layout cache off.
Headless Chromium rasters a 2D canvas on the main thread at its paint and
composites in software, which a desktop with a GPU does not, so every
figure is also taken with `--gpu` (SwiftShader standing in for a GPU):
the software rows are the pessimistic bound, the GPU rows the likelier
one. `paint` is hash change → the frame after `graph:built`; `draw` is
`graph:layout` → that frame (the surface's share, the page's other panels
included); `settle` is a wheel zoom's settle timer → the frame after the
last settle step (the SVG surface's second 160 ms timer included: its
settled picture is not on screen before).


**Every table below was rerun for the parts round** (before = 8cd0dff, the
first canvas commit; now = the working tree; same stores, same release
binary, `--assets` from each tree). Columns added to `views.mjs` then:
`pan settle` (the pointerup that ends a drag → the frame that shows the
settled drawing), `filter toggle` (the Proposed chip off and on: click →
the frame that shows it, mean of the two), `settle, memory selected` (a
wheel zoom settling with a memory selected, so the faded copy is what is
redrawn), long-task counts (PerformanceObserver `longtask`, > 50 ms) in
the 1 s after first paint and around the toggles, and `canvas MB` (every
canvas the host holds, 4 bytes per device pixel, `graphDebug.backingBytes`;
not exposed before). `settle` now counts the settle step's microtasks and
the frame after it (rAF callbacks, paint, and the raster a canvas pays
there), not its synchronous script alone, so it reads higher than the
previous version of these tables for both columns. `views.mjs` waits for
the dashboard to answer before loading the page. The SVG surface (cb5f753),
same harness, for comparison: s1k / s10k show all, software, DPR 1: first
paint 122 / 167, settle 197 / 198, pan settle 153 / 152, filter toggle
40 / 33, settle with a memory selected 187 / 185, hover frame 7.8 / 7.5;
DPR 2: 133 / 183, 197 / 200, 151 / 151, 35 / 36, 193 / 187, 9.4 / 7.8.

**Software raster (headless default)** (8cd0dff → now, 5 runs)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | heap MB | DOM nodes | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cold | 109 → 104 | 41.2 → 38.7 | 18.8 → 19.7 | 17 → 16.4 | 30.5 → 26.4 | 69.7 → 44.7 | 60.0 → 60.0 | 60.0 → 60.0 | 4.4 → 4 | 3.5 → 3.6 | 4939 → 4945 | – → 28.9 |
| s2k show all | cold | 110 → 110 | 40.3 → 39.2 | 19.5 → 21.4 | 24.2 → 22 | 46.8 → 33.8 | 73.8 → 46.9 | 60.0 → 60.0 | 60.0 → 60.0 | 4.9 → 4 | 3.6 → 3.7 | 5143 → 5149 | – → 29.1 |
| s10k opened scope | cold | 150 → 143 | 39.3 → 34.5 | 18.5 → 17.8 | 25.1 → 21.6 | 38.6 → 35.0 | 66 → 46 | 60.0 → 60.0 | 60.0 → 60.0 | 5 → 4.7 | 3.7 → 3.8 | 3580 → 3586 | – → 30.4 |
| s10k show all | cold | 146 → 149 | 34.6 → 31.6 | 18.9 → 18.2 | 26.5 → 22.2 | 37.5 → 29.5 | 59.9 → 43.3 | 60.0 → 60.0 | 60.0 → 60.0 | 5 → 4 | 3.4 → 3.4 | 3378 → 3384 | – → 28.6 |
| s1k show all | cached | 76.6 → 71 | 40.3 → 37.3 | 19.7 → 20.3 | 17.8 → 17 | 34.9 → 27.5 | 70.7 → 47.2 | 60.0 → 60.0 | 60.0 → 60.0 | 5 → 4.1 | 3.5 → 3.6 | 4939 → 4945 | – → 28.9 |
| s2k show all | cached | 81.1 → 77 | 40 → 35.8 | 20.3 → 27.8 | 25.5 → 20.7 | 50.8 → 32.9 | 75.1 → 50.6 | 60.0 → 60.0 | 60.0 → 60.0 | 5.5 → 4 | 3.6 → 3.7 | 5143 → 5149 | – → 29.1 |
| s10k opened scope | cached | 122 → 123 | 38.2 → 36.3 | 19.5 → 22.3 | 26.8 → 23.1 | 40.0 → 36.9 | 78.9 → 42.6 | 60.0 → 60.0 | 60.0 → 60.0 | 5.6 → 5.1 | 3.7 → 3.8 | 3580 → 3586 | – → 30.4 |
| s10k show all | cached | 116 → 112 | 34.9 → 33.8 | 17.6 → 17.3 | 22.4 → 20.8 | 37.5 → 28.3 | 65.9 → 42.8 | 60.0 → 60.0 | 60.0 → 60.0 | 4.9 → 3.7 | 3.4 → 3.4 | 3378 → 3384 | – → 28.6 |

**Software raster, CPU ÷4** (8cd0dff → now, 3 runs; DPR 2 software: 5)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 272 → 238 | 170 → 146 | 81.2 → 74.7 | 88.3 → 57.3 | 152 → 125 | 300 → 200 | 60.0 → 60.0 | 54.1 → 54.9 | 18.3 → 19 | 1 → 0 | 4 → 2 | – → 28.9 |
| s2k show all | cached | 258 → 265 | 173 → 166 | 80.9 → 82.2 | 116 → 103 | 188 → 177 | 322 → 221 | 60.0 → 60.0 | 59.1 → 60.0 | 21.4 → 18.2 | 1 → 0 | 4 → 3 | – → 29.1 |
| s10k opened scope | cached | 315 → 290 | 167 → 154 | 72.1 → 72.6 | 118 → 107 | 197 → 164 | 296 → 185 | 60.0 → 60.0 | 59.1 → 58.2 | 21.9 → 19.6 | 1 → 0 | 4 → 4 | – → 30.4 |
| s10k show all | cached | 337 → 289 | 170 → 144 | 72.4 → 84.4 | 98.6 → 122 | 160 → 128 | 271 → 192 | 60.0 → 60.0 | 60.0 → 59.1 | 17.7 → 21.4 | 1 → 0 | 3 → 2 | – → 28.6 |

**GPU (SwiftShader)** (8cd0dff → now, 3 runs; DPR 2 software: 5)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 62.8 → 57.4 | 22.7 → 23.9 | 12.6 → 15.2 | 17.9 → 16.6 | 26.0 → 33.7 | 19.4 → 19.5 | 60.0 → 60.0 | 60.0 → 60.0 | 4.6 → 3.8 | 0 → 0 | 0 → 0 | – → 28.9 |
| s2k show all | cached | 61.7 → 69.1 | 23.7 → 26.5 | 12.3 → 13.8 | 17.3 → 17 | 26.8 → 24.9 | 18.8 → 14.8 | 60.0 → 60.0 | 60.0 → 60.0 | 5.1 → 4.1 | 0 → 0 | 0 → 0 | – → 29.1 |
| s10k opened scope | cached | 112 → 100 | 27.8 → 23.9 | 10.4 → 10.7 | 16.7 → 16.8 | 31.0 → 22.4 | 15.1 → 13.1 | 60.0 → 60.0 | 60.0 → 60.0 | 4.6 → 4 | 0 → 0 | 0 → 0 | – → 30.4 |
| s10k show all | cached | 95.5 → 93.3 | 20.8 → 20.6 | 11.4 → 13.6 | 16.9 → 17.3 | 24.5 → 27.3 | 15.6 → 14.1 | 60.0 → 60.0 | 60.0 → 60.0 | 3.6 → 4 | 0 → 0 | 0 → 0 | – → 28.6 |

**GPU (SwiftShader), CPU ÷4** (8cd0dff → now, 3 runs; DPR 2 software: 5)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 206 → 205 | 110 → 101 | 48.7 → 49.3 | 50.5 → 42.7 | 87.4 → 97.2 | 72.4 → 62.7 | 60.0 → 60.0 | 57.4 → 54.9 | 21.5 → 17.6 | 0 → 1 | 1 → 2 | – → 28.9 |
| s2k show all | cached | 213 → 210 | 110 → 106 | 52.7 → 50.2 | 33.8 → 25.3 | 99.9 → 111 | 67.3 → 70.9 | 60.0 → 60.0 | 60.0 → 60.0 | 20.3 → 19.4 | 0 → 0 | 1 → 2 | – → 29.1 |
| s10k opened scope | cached | 276 → 253 | 122 → 108 | 45.1 → 40.2 | 33.8 → 37.2 | 136 → 100 | 66.9 → 66.1 | 60.0 → 60.0 | 58.2 → 59.1 | 24.2 → 20.4 | 0 → 0 | 2 → 1 | – → 30.4 |
| s10k show all | cached | 248 → 234 | 101 → 102 | 46.5 → 45.9 | 32.8 → 23.8 | 89.9 → 84.2 | 60.7 → 57.5 | 60.0 → 60.0 | 60.0 → 59.2 | 19.6 → 16.1 | 0 → 1 | 1 → 1 | – → 28.6 |

**Software raster, DPR 2** (8cd0dff → now, 3 runs; DPR 2 software: 5)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 108 → 98.7 | 68.1 → 65.6 | 29.2 → 28.3 | 28.3 → 23.2 | 61.8 → 47.6 | 131 → 105 | 60.0 → 60.0 | 60.0 → 60.0 | 7.6 → 6.5 | 1 → 0 | 1 → 0 | – → 116 |
| s2k show all | cached | 115 → 108 | 72.8 → 63 | 33.5 → 28.4 | 51.9 → 71.9 | 85.5 → 71.5 | 147 → 114 | 60.0 → 60.0 | 59.0 → 58.1 | 7.5 → 6.9 | 1 → 0 | 2 → 1 | – → 117 |
| s10k opened scope | cached | 153 → 148 | 72.2 → 65.2 | 29.1 → 28.5 | 54.6 → 79.7 | 88.9 → 82.9 | 136 → 96.6 | 60.0 → 60.0 | 59.1 → 58.2 | 7.9 → 8.5 | 1 → 0 | 3 → 2 | – → 122 |
| s10k show all | cached | 137 → 139 | 63.9 → 55.9 | 25.6 → 26 | 45.7 → 74.1 | 72.0 → 70.2 | 121 → 88.2 | 60.0 → 60.0 | 60.0 → 58.1 | 7.1 → 7.1 | 1 → 0 | 1 → 1 | – → 114 |

**GPU (SwiftShader), DPR 2** (8cd0dff → now, 3 runs; DPR 2 software: 5)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 64.9 → 62.4 | 26.4 → 25 | 13.5 → 12.5 | 17.6 → 16.8 | 26 → 25.1 | 20.7 → 19 | 60.0 → 60.0 | 60.0 → 60.0 | 6.1 → 4.5 | 0 → 0 | 0 → 0 | – → 116 |
| s10k show all | cached | 106 → 111 | 25.2 → 21.9 | 15.3 → 11.6 | 16.7 → 17 | 31.6 → 35.2 | 16.8 → 16.7 | 60.0 → 60.0 | 60.0 → 60.0 | 6.2 → 4.9 | 0 → 0 | 0 → 0 | – → 114 |

**Software raster, DPR 2, CPU ÷4** (8cd0dff → now, 3 runs; DPR 2 software: 5)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 421 → 387 | 321 → 284 | 117 → 122 | 126 → 125 | 296 → 238 | 628 → 464 | 60.0 → 58.5 | 58.4 → 58.0 | 33.1 → 31.8 | 1 → 6 | 5 → 6 | – → 116 |
| s10k show all | cached | 448 → 451 | 303 → 302 | 110 → 106 | 233 → 371 | 352 → 292 | 577 → 376 | 60.0 → 57.8 | 58.7 → 58.9 | 32.8 → 29.7 | 1 → 6 | 4 → 5 | – → 114 |

**What moved.** A settle is now one synchronous redraw in the task that
removes the gesture's transform: 184–255 → 13–28 ms (the SVG waited a
second timer and rebuilt relations). Pan, which the SVG surface did by
writing the view box per move, is a composited gesture too: 20–27 → 60 fps
with a GPU, 29–45 → 56–58 at CPU ÷4. Zoom at CPU ÷4 is 59–60 (target
≥ 50). The DOM halves (no element per relation; one invisible button per
memory), and the hover frame is lower everywhere at DPR 1 (7–9 ms with a
GPU, under the 12 ms target; 6.2–6.9 in software). First paint is
30–40 % faster in software at DPR 1 and 5–20 % at DPR 2.

**Draw time against the spike.** The spike's 31–39 ms and the first
version's 75–95 ms measured different spans. The spike timed a canvas
created and drawn from precomputed data in a bare harness; `draw` runs from
`graph:layout` to the first task after the frame that shows the graph, so
it also holds building the page around the graph (the accessible list,
legend, relations table, card), the style and layout of all of it, the
canvas raster (software raster runs on the main thread at the frame) and
the frame itself. s1k show all, DPR 1, first version (~88 ms): surface
construction 9, view + legend 3, relations (records and drawing) 24, of
which 5.9 was a style recalculation the first `ctx.font` forced; the
relations table and card 5.5; style 3 + layout 15 (mostly the 100-row
relations table and the 800 buttons); raster 13; the rest of the frame 14.
Now (s2k show all, DPR 1, medians of 15): layout → view + legend 4.6,
relation records 2.1, region paths 2.8, regions 0.7, relations 2.9, names
1.4, labels 1.3, memories 2.0, then the frame 24 (raster ~15 of it); 43 ms
in all. What cut it, each measured (21-run medians unless noted):

| choice | kept? | effect |
|---|---|---|
| relations table built after the first paint (`afterPaint`: two frames, then idle) | kept | −13 ms layout (`content-visibility: auto` instead: no gain, Chrome laid it out in the frame anyway) |
| accessible list built after the first paint, or on the first focus into the graph (`ensureList`) | kept | −6 ms style; keyboard input always finds it built |
| legend mounted after the first paint | kept | −3 ms |
| first drawing without the overscan margin, the margin grown once it is on screen by copying the first drawing into the centre and drawing only the strips around it | kept | 8cd0dff drew the base twice in the first-paint task (the margin rule read `key === null`), so this saved nothing there; with a drawn-once flag, measured against a build that draws the margin from the start (9 runs, cached): draw s1k / s2k 42.2 / 41.2 → 37.0 / 41.4 ms at DPR 1, 64.8 / 66.5 → 59.4 / 59.3 at DPR 2 |
| relations written straight into one Path2D per group through a sink (`relationShape`), and a fast path for solid bands | kept | identical path data on 5,760 cases; −5 ms |
| no `closePath` in a group's path (a fill closes each subpath itself) | kept | Chrome's `Path2D.closePath` costs time in proportion to the path so far: 10k points with 667 closes 10 ms, without 0.7; relations 9.3 → 5.0 ms |
| the relation drawing run on made-up relations while the page is idle (`warmStep`), so the first graph finds it optimised | kept | relations 5.0 → 2.9 ms (cold, 610 relations cost 9 ms of JS; warm, under 1) |
| region paths built with `arc` instead of parsing `regionArcs`' path data | kept | 51.9 → 50.0 ms (within noise; simpler) |
| keyboard walk order sorted on the first `]`, not at build | kept | −1.3 ms at the 10k opened scope |
| per-disc opaque region tints (pre-blended over the background) | dropped | 3.1 → 1.9 ms raster in a micro-bench, but where two scopes overlap the later one would hide the earlier: a visible change for ~1 ms |
| name halos (`strokeText`) | kept | dropping them saved ~1 ms; not worth the look |
| base raster in an OffscreenCanvas worker, page fallback | dropped | built and measured: first paint 53 → 96 ms (s1k, DPR 1); the worker's module graph arrives ~20 ms after the layout, its JIT and fonts start cold, and the raster stays on the critical path |
| Path2D batches cached across draws | kept (already) | a settle that only moves the view reuses the merged paths |

Not cut: the 10k opened scope spends 3.4 ms laying out the scope overview
on the main thread for its tags' directions (shared with the SVG surface,
cached when the page comes from the overview).

**DPR 2.** First paint is now below the SVG at every store (above). The
hover frame was 20–23 ms; it is 9.1–10.4, below the SVG's 10.1–11.7:

| choice | kept? | effect (hover frame, DPR 2, software) |
|---|---|---|
| a small patch canvas over the lit bounds instead of a full-size lit overlay, with the faded labels and memories in the faded layer | kept | part of 20–23 → 15 ms |
| accessible list clipped from paint (`contain: paint`) unless one of its buttons has focus | kept | 15.3 → 8.6 ms (s1k). Its 800 positioned buttons made every hover frame re-layerize the page (7.8 ms per frame in a trace); a focused button keeps its bounds for a screen magnifier |
| far-scope tags drawn in the faded layer, so only a marked tag widens the patch | kept | 10k opened scope 21.6 → 10.5 ms (every tag was in the patch, making it the whole box) |
| patch drawn into the faded layer itself (no layer of its own) | dropped | 8.6 → 8.6 ms once the list was clipped (5.9 → 5.0 at DPR 1) |
| patch canvas sized in 64-px steps (fewer reallocations) | dropped | 8.6 → 9.4 ms (noise) |
| list items placed in one grid cell by margins (not positioned) | dropped | hover 36–40 ms: a grid relayout per hover |
| `will-change` on the tooltip, canvases or pane; tooltip shadow off | dropped | no change (14.5–16.9 ms before the list fix) |

### Parts round (fix round on 8cd0dff)

Software raster, s1k show all unless noted, before → with the choice (or,
for a dropped one, kept → dropped). A software blit is about 2.5 ms per
million device pixels (measured in the page: 9–12 ms for one 2268 × 1640
alpha blit, 7 for an opaque copy), which decided most of these.

| choice | kept? | effect |
|---|---|---|
| layers composed from part bitmaps (below, edges, above, labels, nodes), each with its own version and frame key | kept | filter toggle 35 → 28 ms (DPR 1), 62 → 48 (DPR 2); settle with a memory selected 71 → 47 (DPR 1), 131 → 105 (DPR 2) |
| an `over` part (above + labels + nodes) and the relations drawn straight into the base, so the base is two blits, not five | kept | DPR 2 toggle, the host's base bring-up 30–35 → 17 ms (its own timing) |
| the base drawn straight from the scene at a new frame whose parts are behind (a settle), the parts following in idle time | kept | settle 28 → 20 ms (DPR 1), 50 → 34 (DPR 2) against composing it from freshly drawn parts |
| the hidden layer brought up in idle time a part per callback, then in bands of 1.2 M device px, each rastered through a 1 × 1 sink | kept | the hidden layer no longer rasters at the focus frame; long tasks in the 1 s after first paint 1 → 0 (DPR 2, software); at DPR 2 ÷4 they are 1 → 6 (more, shorter steps: each part still exceeds 50 ms there) |
| pan strips each under a rectangle clip, not one even-odd clip | kept | DPR 2 pan, the base bring-up 91 → 47 ms |
| unfaded pan strips drawn straight from the scene (one canvas copied, not every part) | kept | DPR 2 pan, base bring-up 47 → 5 ms; pan settle s1k 28 → 23 ms |
| faded pan strips drawn from the scene through strip-sized scratch parts | dropped | s2k / s10k pan settle at DPR 2 59 → 88 ms (the JS walks every item five times per strip) |
| memory sprites per type / look / radius bucket | not built | memories cost 1–2 ms per draw in a profile of six toggles (drawNodes 6.9 ms in all); the gain is bounded there, and the pixel range against the SVG was the risk |
| `ImageBitmapRenderingContext` for the shown layer (`transferFromImageBitmap` of an OffscreenCanvas) | dropped | a 2268 × 1640 update + frame: 5.0–5.2 ms with a 2D canvas, 14.8–16.7 with the bitmap renderer |
| stubs batched | not built | not visible in the profiles (≤ 200 stubs in an opened scope); the look would need its own pixel check |
| `toScreen` reads the box size the view keeps (`boxSize`), and the patch canvas only grows | kept | no forced layout per hover (not measured alone; the whole round moved the hover frame 5.0 → 4.1 ms at DPR 1, 7.6 → 6.5 at DPR 2) |
| scratch canvases freed after use (spare copy canvas, 1 × 1 sink) | kept | canvas MB at DPR 2: 114–122 (base, dim, patch and six parts at 2268 × 1640; the `over` part is ~15 of it) |

**Not reached.** The brief's targets of a 10–15 ms faded-copy rebuild and a
≤ 25 ms filter toggle at DPR 2 were set before the blit cost was measured:
two full-size blits at DPR 2 are ~20 ms in software before any relation is
drawn, so the toggle is 48–83 ms there (SVG 35–36). At DPR 1 it is 26–37
(SVG 33–40). Drawing the relations on a canvas of their own between two
static ones would make a toggle a clear and a redraw, at the price of a
third compositor layer every frame; not built. **Regression:** a pan
settle with a memory selected on the 2k / 10k stores at DPR 2 is 46–55 →
72–80 ms (the faded copy's six part copies plus its own), and at DPR 2 ÷4
233 → 371 on s10k.

### DPR 2 round (fix round on 945329e)

Targets, software raster at DPR 2: a filter toggle ≤ 35 ms (s1k, s2k, s10k
opened), a pan settle ≤ 55 ms on s2k / s10k, ≤ 1 long task after paint at
DPR 2 ÷4, ≤ 60 canvas MB. Same stores, release binary and `views.mjs`; before
= 945329e's `web/`, now = the working tree; 5 runs (3 for ÷4 and GPU). All
seven tables were rerun:

**Software raster, DPR 1** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cold | 122 → 110 | 38.6 → 39.3 | 24.9 → 19.5 | 17.5 → 15.1 | 29.9 → 16.0 | 47.6 → 49.3 | 60.0 → 60.0 | 60.0 → 60.0 | 4.6 → 3.8 | 0 → 0 | 0 → 0 | 28.9 → 17.9 |
| s2k show all | cold | 119 → 118 | 39.7 → 40.5 | 22.1 → 20.4 | 22.6 → 16.4 | 41.3 → 18 | 50.9 → 45.8 | 60.0 → 60.0 | 60.0 → 60.0 | 5.3 → 3.9 | 0 → 0 | 0 → 0 | 29.1 → 18.0 |
| s10k opened scope | cold | 183 → 152 | 43.3 → 37.7 | 25.5 → 17.4 | 27.8 → 16.1 | 40.8 → 20.7 | 47.9 → 37.7 | 60.0 → 60.0 | 60.0 → 60.0 | 5.7 → 3.4 | 0 → 0 | 0 → 0 | 30.4 → 17.9 |
| s10k show all | cold | 176 → 157 | 38.3 → 34.6 | 18.4 → 16.3 | 24.3 → 16.3 | 35.4 → 18.1 | 54.5 → 33.3 | 60.0 → 60.0 | 60.0 → 60.0 | 5.4 → 2.8 | 0 → 0 | 0 → 0 | 28.6 → 17.7 |
| s1k show all | cached | 75.3 → 74.5 | 38.6 → 37.9 | 23.2 → 19.5 | 18.5 → 16.1 | 28.9 → 16.8 | 54.3 → 38.3 | 60.0 → 60.0 | 60.0 → 60.0 | 5 → 3.1 | 0 → 0 | 0 → 0 | 28.9 → 17.9 |
| s2k show all | cached | 86.1 → 82.6 | 43.6 → 40.9 | 24.1 → 19.7 | 22.6 → 16.4 | 39.3 → 18.5 | 49.5 → 51.7 | 60.0 → 60.0 | 60.0 → 60.0 | 5.3 → 4 | 0 → 0 | 0 → 0 | 29.1 → 18.0 |
| s10k opened scope | cached | 124 → 133 | 41.1 → 42.4 | 18.9 → 20.3 | 24.5 → 16.1 | 37.1 → 22.1 | 45.2 → 41.2 | 60.0 → 60.0 | 60.0 → 60.0 | 5.2 → 3.4 | 0 → 0 | 0 → 0 | 30.4 → 17.9 |
| s10k show all | cached | 111 → 121 | 35 → 36.4 | 18.1 → 19 | 22.3 → 16.2 | 31.8 → 17.5 | 42.8 → 33.7 | 60.0 → 60.0 | 60.0 → 60.0 | 4.4 → 3.3 | 0 → 0 | 0 → 0 | 28.6 → 17.7 |

**Software raster, DPR 1, CPU ÷4** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 288 → 264 | 183 → 164 | 83.9 → 77.3 | 60.2 → 50 | 128 → 49.2 | 214 → 164 | 60.0 → 60.0 | 57.4 → 55.0 | 20.2 → 13.4 | 0 → 1 | 2 → 0 | 28.9 → 17.9 |
| s2k show all | cached | 311 → 317 | 194 → 200 | 91.5 → 80.2 | 110 → 41.2 | 161 → 71.2 | 241 → 193 | 60.0 → 60.0 | 59.1 → 59.1 | 20.2 → 14.9 | 0 → 1 | 3 → 1 | 29.1 → 18.0 |
| s10k opened scope | cached | 328 → 341 | 174 → 175 | 77 → 66.2 | 113 → 38.9 | 169 → 81.0 | 202 → 170 | 60.0 → 60.0 | 59.1 → 59.1 | 24.3 → 13.2 | 0 → 1 | 4 → 3 | 30.4 → 17.9 |
| s10k show all | cached | 319 → 301 | 161 → 159 | 76.1 → 72.5 | 102 → 46.3 | 135 → 64.0 | 181 → 152 | 60.0 → 60.0 | 60.0 → 59.1 | 18.1 → 12.3 | 0 → 1 | 3 → 0 | 28.6 → 17.7 |

**GPU (SwiftShader), DPR 1** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 64.4 → 61.5 | 25.5 → 27 | 14.5 → 10.6 | 16.5 → 15.8 | 24.8 → 12.3 | 18.8 → 12.1 | 60.0 → 60.0 | 60.0 → 60.0 | 4.1 → 2 | 0 → 0 | 0 → 0 | 28.9 → 17.9 |
| s2k show all | cached | 66.7 → 65.3 | 26.4 → 26.4 | 12.9 → 13 | 16.8 → 15.8 | 27.9 → 17.6 | 20 → 14.6 | 60.0 → 60.0 | 60.0 → 60.0 | 4.3 → 2.6 | 0 → 0 | 0 → 0 | 29.1 → 18.0 |
| s10k opened scope | cached | 118 → 129 | 31 → 28.6 | 10.9 → 9.4 | 17.3 → 16 | 29.8 → 20.4 | 14 → 14.9 | 60.0 → 60.0 | 60.0 → 60.0 | 4.9 → 2.6 | 0 → 0 | 0 → 0 | 30.4 → 17.9 |
| s10k show all | cached | 106 → 107 | 24.3 → 26.6 | 11 → 9.6 | 17.6 → 15.8 | 26.1 → 10.4 | 13.3 → 15.9 | 60.0 → 60.0 | 60.0 → 60.0 | 3.4 → 2.1 | 0 → 0 | 0 → 0 | 28.6 → 17.7 |

**GPU (SwiftShader), DPR 1, CPU ÷4** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 203 → 228 | 99.1 → 116 | 51.7 → 38 | 39.7 → 31.1 | 93.7 → 37.1 | 73.6 → 49.2 | 60.0 → 60.0 | 57.3 → 59.1 | 15.1 → 12.4 | 0 → 0 | 1 → 0 | 28.9 → 17.9 |
| s2k show all | cached | 208 → 245 | 107 → 117 | 63.1 → 71 | 25.2 → 16.7 | 104 → 53.1 | 93.4 → 58 | 60.0 → 60.0 | 56.8 → 60.0 | 16.5 → 13.8 | 0 → 0 | 1 → 0 | 29.1 → 18.0 |
| s10k opened scope | cached | 267 → 289 | 119 → 135 | 52.1 → 48.1 | 32.9 → 19.2 | 103 → 70.6 | 65 → 72.3 | 60.0 → 60.0 | 59.1 → 60.0 | 19.4 → 12.1 | 0 → 0 | 1 → 0 | 30.4 → 17.9 |
| s10k show all | cached | 282 → 251 | 101 → 102 | 47.5 → 36.6 | 27 → 17.3 | 92.4 → 32.7 | 61.8 → 44.9 | 60.0 → 60.0 | 58.2 → 60.0 | 16.5 → 10.8 | 1 → 0 | 1 → 0 | 28.6 → 17.7 |

**Software raster, DPR 2** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 92.3 → 98 | 57.8 → 61.4 | 28.8 → 25 | 22.9 → 18.1 | 47.6 → 20.8 | 89 → 72.9 | 60.0 → 60.0 | 60.0 → 60.0 | 6.5 → 5 | 0 → 0 | 0 → 0 | 116 → 58.1 |
| s2k show all | cached | 107 → 110 | 65.2 → 66.7 | 28.4 → 27.5 | 72.3 → 17.1 | 69.4 → 30.7 | 112 → 91.1 | 60.0 → 60.0 | 58.1 → 60.0 | 7.2 → 5.7 | 0 → 0 | 1 → 0 | 117 → 58.4 |
| s10k opened scope | cached | 158 → 164 | 75.7 → 72.1 | 28.9 → 28.2 | 78.6 → 16.7 | 76.7 → 41.1 | 94.6 → 80.7 | 60.0 → 60.0 | 58.1 → 60.0 | 7.8 → 4.8 | 0 → 0 | 1 → 0 | 122 → 58.2 |
| s10k show all | cached | 147 → 145 | 56.2 → 58.8 | 24.3 → 22.7 | 71.8 → 16.4 | 60.9 → 25.6 | 84 → 62.6 | 60.0 → 60.0 | 58.1 → 60.0 | 6.2 → 4.5 | 0 → 0 | 0 → 0 | 114 → 57.4 |

**GPU (SwiftShader), DPR 2** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 67.3 → 59.6 | 29.4 → 26.6 | 12.2 → 27.9 | 17.1 → 15.7 | 24.5 → 16.4 | 17.2 → 15.9 | 60.0 → 60.0 | 60.0 → 60.0 | 5 → 3.5 | 0 → 0 | 0 → 0 | 116 → 58.1 |
| s2k show all | cached | 71 → 72.6 | 29 → 29.3 | 13.2 → 22.6 | 17.2 → 16.1 | 47.6 → 18.2 | 16 → 22.1 | 60.0 → 60.0 | 60.0 → 60.0 | 5 → 4.2 | 0 → 0 | 1 → 0 | 117 → 58.4 |
| s10k show all | cached | 112 → 111 | 23.7 → 25.6 | 13 → 12.7 | 16.5 → 15.8 | 32.1 → 17.8 | 14.1 → 12.7 | 60.0 → 60.0 | 60.0 → 60.0 | 4.3 → 2.7 | 0 → 0 | 0 → 0 | 114 → 57.4 |
| s10k opened scope | cached | 111 → 112 | 31 → 28.3 | 20.4 → 16.2 | 17.4 → 15.8 | 42.0 → 18 | 17.2 → 17.4 | 60.0 → 60.0 | 60.0 → 60.0 | 5.4 → 2.9 | 0 → 0 | 0 → 0 | 122 → 58.2 |

**Software raster, DPR 2, CPU ÷4** (945329e → now)

| view | mode | first paint | draw | settle | pan settle | filter toggle | settle, memory selected | zoom fps | pan fps | hover frame | long tasks 1 s after paint | long tasks around the toggles | canvas MB |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| s1k show all | cached | 386 → 377 | 275 → 273 | 118 → 102 | 115 → 99.9 | 222 → 82.4 | 423 → 368 | 60.0 → 60.0 | 58.4 → 58.2 | 29.9 → 22.5 | 6 → 1 | 6 → 1 | 116 → 58.1 |
| s2k show all | cached | 421 → 403 | 304 → 297 | 116 → 120 | 352 → 81.3 | 327 → 133 | 454 → 434 | 59.2 → 60.0 | 58.8 → 55.7 | 32.5 → 24.8 | 7 → 3 | 5 → 4 | 117 → 58.4 |
| s10k opened scope | cached | 439 → 495 | 279 → 300 | 127 → 124 | 405 → 91 | 364 → 197 | 427 → 401 | 60.0 → 60.0 | 58.9 → 58.4 | 36 → 23.4 | 7 → 2 | 5 → 4 | 122 → 58.2 |
| s10k show all | cached | 402 → 426 | 250 → 279 | 102 → 124 | 345 → 83.3 | 283 → 114 | 363 → 292 | 60.0 → 60.0 | 58.9 → 57.0 | 25.6 → 20.6 | 6 → 1 | 5 → 2 | 114 → 57.4 |

**Targets.** Filter toggle at DPR 2: 47.6 / 69.4 / 76.7 → 20.8 / 30.7 /
41.1 ms (s1k / s2k / s10k opened; s10k show all 60.9 → 25.6): s1k and s2k
met, the 10k opened scope **missed by 6 ms**. Pan settle at DPR 2: 72–79 →
16–18 ms (met; at ÷4 s10k show all 345 → 83, under 233). Long tasks after
paint at DPR 2 ÷4: 6–7 → 1–3 (met on s1k and s10k show all, **missed** on
s2k (3) and the 10k opened scope (2)). Canvas MB at DPR 2: 114–122 → 57–58
(met).

| choice | kept? | effect |
|---|---|---|
| per-rectangle drawing: every layer and part keeps the rectangles it owes (`pend`), a flush draws only the shown layer's box, the margin, parts and hidden layer follow in an idle pump | kept | DPR 2 pan settle 72–79 → 16–18 ms (no layer is redrawn whole in the settle task) |
| two parts, `below` and `over`, instead of six; the faded copy drawn from the scene through a rect-sized scratch | kept | canvas MB 114–122 → 47 (four full-size canvases), with LAYER_PX = 3 M device px capping the margin |
| a `faint` part (labels and memories, each faded as a group) for the faded copy | kept | a toggle under a hover (the bench's pointer rests on the box): s2k 68 → 32 ms, s10k opened 66 → 47 (quick runs); hover frame 8.9–11.9 → 6.1–7.5; canvas MB 47 → 58 |
| patch underlay copied from the shown layer, the lit label boxes redrawn without their faded labels (`labelHoles`) | kept | no separate time change measured (the copy's cost is the faded box's raster, which moves to the snapshot); simpler than redrawing the patch from the scene |
| relation groups keyed by kind × {rejected, proposed, neither}, each group's Path2D cached by record identity | kept | a filter rebuilds no path; part of the toggle numbers above |
| memory-look changes (`setHub`) owed as discs a double rAF later, drawn with only what reaches them | kept | one rAF landed in the measured frame; the merged-bounds redraw (PEND_MAX 24) was the slow first version |
| stubs in `below`, tags in `over` | dropped | the 10k opened toggle got slower: a filter changes both, so both parts were redrawn |
| background-priority tasks once an idle callback times out (`bgCall`) | kept | headless software compositing grants no idle period while nothing changes: the pump ran only at its 500 ms timeout and finished 1.5–6 s after a settle; now ~0.2 s. Without it a toggle 500 ms after a zoom found its parts behind (s2k 68 → 48 ms) |
| parts first in the pump | kept | a toggle or hover meanwhile composes instead of drawing from the scene |
| a kind's first band (its canvas's allocation) the smallest, alone in its callback; bands sized at 1.5× the measured cost, none started with < 12 ms left; ROOM_MS 30 | kept | pump tasks at ÷4 42–61 → under 50 (traced) |
| the margin in the frame from the first drawing (owed to idle time) | kept | the old margin-adding flush (a canvas resize + spare copy) was a 55–68 ms task at DPR 2 ÷4; first paint at DPR 1 GPU ÷4 s2k 212 → 231 ms in one A/B (s10k opened 286 → 288), within this harness's noise but the likely share of the D-table first-paint rise |
| relations table rows after the first 10 built in later idle slices; accessible list slices 200 → 60 | kept | the table's 60 ms script and each slice's frame (layout + paint + commit ~50–70 ms at ÷4) were the remaining long tasks |
| `content-visibility: auto` on the table box | dropped | the table sits at the viewport edge, so Chrome laid it out anyway (dirty objects unchanged) |
| faded copy at a lower DPR | not tried | the faint part brought the hover toggle under target without a pixel change |

**Why the misses.** The 10k opened scope's toggle under a hover redraws the
faded box: of its ~17 ms raster, ~9 ms is the faded stubs (365 strokes
at 0.08 alpha, each its own composite to match the SVG's per-element
opacity; batching them would change where stubs overlap near a tag).
Without a hover it is 29–45 ms. The remaining long tasks at ÷4 are frames:
each idle slice that adds DOM (list buttons, table rows) is followed by a
frame of style + layout + paint + an 8–9 ms canvas commit, 50–58 ms at ÷4.
**Regressions:** first paint at DPR 1 GPU ÷4 +10–18 % (203–267 → 228–289;
see the margin row) and at DPR 2 ÷4 on the 10k store +6–13 % (402–439 →
426–495); DPR 2 GPU settle s1k / s2k 12–13 → 23–28 ms (the box drawn from
the scene where the parts are behind, which a GPU blit used to hide).

### Compositing choices, measured

s10k show all, cached, `--gpu --cpu-throttle 4` unless noted, 3 runs;
before → with the choice (or, for a dropped one, kept → dropped).

| choice | kept? | effect |
|---|---|---|
| composited gesture for pan as well as zoom, settle drawn synchronously | kept | pan 35 → 58 fps; settle 414 → 92 ms |
| overscan margin 12 % of the box's larger side | kept | 0 %: settle 70 → 81 ms, pan 59 → 58, and a pan shows blank edges; 25 %: settle 82, paint −16 ms (noise); fps the same |
| relations batched, one Path2D per kind × rejected (`addPath` with each relation's matrix) | kept | per-relation fills: paint 524 → 644 ms, draw 353 → 376 |
| opaque base (`alpha: false`) | kept | `alpha: true`: paint 524 → 637 ms, draw 353 → 418 |
| `desynchronized: true` | dropped | paint 524 → 639 ms, hover 36 → 41 ms; tears on some platforms |
| DPR capped at 2 and 6 M device px | kept | uncapped at DPR 2: paint 584 → 718 ms, hover 75 → 83; caps of 3 M / 4 M changed no fps (software, DPR 2) |
| accessible list clipped from paint mid-gesture (`contain: paint`; now also whenever none of its buttons has focus, see DPR 2 above) | kept | zoom at DPR 2, software, CPU ÷4: 16.6 → 53.8 fps. With the box focused its 800 positioned buttons made Chrome re-layerize every frame. Clipped permanently: same zoom, hover 153 → 100 ms, but a screen magnifier would lose the focused memory's position, so only mid-gesture |
| one canvas mid-gesture (`surface.gesture`: the overlay drawn into the layer under it) | kept | pan at DPR 2, software, CPU ÷4: 23 → 56–58 fps (every visible canvas re-sends its backing store each frame in software compositing) |
| at most two canvases in the page (faded labels / memories blitted into the overlay from off-page scratch canvases instead of their own layers) and the overlay redrawn only in the old ∪ new rectangle (since replaced by the patch canvas, see DPR 2 above) | kept | hover frame at DPR 2: software 31.5–34.7 → 20.2–23.0 ms, SwiftShader 21.8 → 14.4–18.0; CPU ÷4 software 137–147 → 92–105 |
| empty overlay hidden, not cleared | kept | no measurable effect alone (zoom 19.4 → 19.7 before the list fix); part of the two-canvas rule |
| one font string for the scope names (sized by the transform) | kept | −6 ms layout → paint median, within noise (7 runs) |
| layout and drawing in an OffscreenCanvas worker | dropped | built for the base raster and measured slower (draw-time table above); for the overlay it would also put it a frame behind the DOM tooltip and panel it must match |

### Accuracy against the SVG

Screenshots at device scale 2 (`scratchpad` tooling, not committed), old
SVG vs canvas, % of pixels differing by more than 8 / 32 of 255 in any
channel: s60 Fit 1.03 / 0.59 (light), 1.03 / 0.54 (dark); s60 3× Fit
0.72 / 0.43, 0.74 / 0.40; s1k show all Fit 1.04 / 0.28, 0.92 / 0.24; 3×
1.51 / 0.32, 1.42 / 0.24; s10k opened scope Fit 0.43 / 0.09, 0.39 / 0.07;
3× 1.37 / 0.23, 1.28 / 0.19; a focused memory (s60, s1k) 0.64 / 0.08 and
0.39 / 0.07. What differs: text and edge anti-aliasing, and where two
relation kinds cross the canvas stacks by kind (faint first) rather than
by index. Grid, the scope overview and every Grid view (Fit, 3×, light,
dark; s60, s1k show all, s10k opened) are 0.000 %.

After the parts round (DPR 1 screenshots, s60 and s1k show all, Fit and 3×, light
and dark): against the SVG 0.73–1.50 % / 0.23–0.59 %, against 8cd0dff ≤ 0.005 %
/ ≤ 0.001 % (max 96 of 255, a few edge pixels); Grid still 0.000 %.

After the DPR 2 round (DPR 1 screenshots, s60 and s1k show all, Fit and 3×,
light and dark), against 945329e: ≤ 0.014 % of pixels > 8 levels, ≤ 0.003 %
> 32, max 159 of 255 (a few pixels where a kind's proposed relations now stack
over its confirmed ones, the new group order); Grid still 0.000 %.

## Rerun

```bash
cargo run --example dashboard_bench_store -- /tmp/dash-bench
cargo bench -p engram-dashboard --bench dashboard_graph
cargo build --release --bin engramdb
cd crates/engram-dashboard
NODE_PATH=$(npm root -g) node bench/browser/run.mjs --stores /tmp/dash-bench --runs 3 --json /tmp/dash-bench/results.json
```

For the canvas surface (both `web/` versions on one binary):

```bash
NODE_PATH=$(npm root -g) node bench/browser/views.mjs --stores /tmp/dash-bench --assets web --label after --json after.json
NODE_PATH=$(npm root -g) node bench/browser/views.mjs --stores /tmp/dash-bench --assets web --label after --gpu --cpu-throttle 4 --modes cached --json after-gpu-t4.json
NODE_PATH=$(npm root -g) node bench/browser/checks.mjs --stores /tmp/dash-bench --assets web
```

Close other heavy work first: a parallel `cargo build` doubles every
browser figure. Compare runs on one machine only.
