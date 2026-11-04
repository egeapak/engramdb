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
   one machine dir (`<dir>/{data,conf,models}`), in about ten seconds. The
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
loads each), with the worst combination in brackets (slowest time, lowest
fps). Before = branch head before phase H's fixes, after = with them, same
binary, same stores.

| p50 (worst) | s60 | s1k | s2k | s10k | budget |
|---|---|---|---|---|---|
| `/graph`, as the page sees it (fetch + parse) | 14 (17) | 16 (19) | 21 (22) | 47 (56) | 150 / 300 / 400 |
| tab click → painted | 95 (100) | 52 (64) | 60 (71) | 104 (116) | 800 / 1500 / 800 |
| layout cold in the worker, scope-force (Plain) | 16 | 111 | 166 | 179 (2000 of 10k) | 400 / 1000 |
| layout cold in the worker, grid | 18 | 181 | 344 | 375 (2000 of 10k) | 400 / 1000 |
| open the largest scope | – | 148 (182) | 194 (228) | 292 (346) | 300 / 500 / 500 |
| show all (≤ 800 memories) | – | 325 (396) | 333 (442) | 347 (382) | (tab click) |
| filter toggle, before → after | 18 → 19 | 22 → 22 | 26 → 24 | **47 (54) → 36 (40)** | 50 / 100 |
| hover frame, p99 | 17 | 17 | 17 | 17 | 16 (one 60 Hz frame) |
| zoom fps, before → after | 60 → 60 | 60 → 60 | 58 (54) → 60 (60) | **48 (33) → 59 (53)** | 60 (2k ≥ 45) |
| pan fps | 60 | 60 | 60 (59) | 59 (58) | 60 |
| relations, first page | 40 (44) | 72 (88) | 70 (96) | 120 (132) | 200 / 300 |

Rows without "before → after" did not move beyond run-to-run noise (the
fixes touch neither the server, the layouts nor the first build). A hover
frame of 16.7–16.8 ms is one 60 Hz frame: no hover dropped a frame. The
"tab click" at s60 is the whole graph (layout included); from s1k up it is
the overview.

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
  take their width back on settle. Plain at 10k: 33–40 → 58–59 fps.
- **Filter toggle.** A display-filter chip rebuilt every drawn relation
  (path data, element, three listeners) and the browser restyled them all.
  `drawEdges` now keeps the elements it built (keyed by scale, and the view
  in cut mode) and a toggle only sets `hidden` on the ones it drops, built
  lazily the first time they are shown and kept in index order, so what
  overlaps what never changes. 10k opened scope: 45–54 → 34–40 ms.

Grid is pixel-identical before and after (light / dark, 1280 / 390, the
full graph, show-all, an opened scope, after each toggle and after a zoom),
up to the same few anti-aliased pixels two runs of the old code differ by.

Not needed at these sizes, so not built: the sessionStorage layout cache,
better start positions, one path per kind and a canvas renderer for big
opened scopes. Every layout and first-paint figure is under a third of its
budget.

## Still over

- **Grid zoom at 800 memories: 54–56 fps** (Plain 59). Frames drop only at
  scales between Fit and about 3× Fit, where every glyph and its halo is on
  screen at a larger raster scale; zoomed further in the frame rate is 60.
  It is paint-bound in Chrome's software rasterizer (the sandbox has no
  GPU) and within the 2k ≥ 45 fps budget; a GPU-rastered desktop should not
  see it.

## Rerun

```bash
cargo run --example dashboard_bench_store -- /tmp/dash-bench
cargo bench -p engram-dashboard --bench dashboard_graph
cargo build --release --bin engramdb
cd crates/engram-dashboard
NODE_PATH=$(npm root -g) node bench/browser/run.mjs --stores /tmp/dash-bench --json /tmp/dash-bench/results.json
```

Close other heavy work first: a parallel `cargo build` doubles every
browser figure. Compare runs on one machine only.
