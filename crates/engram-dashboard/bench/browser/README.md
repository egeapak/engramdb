# Dashboard browser bench

A developer tool, outside `web/` (never embedded, never scanned by the
frontend tests). The timing benches (`run.mjs`, `views.mjs`) are run by hand
only; `checks.mjs` is also run by CI's `frontend-browser` job (see "In CI"
below). It starts
`engramdb dashboard --no-open` on each benchmark store, drives Chromium with
Playwright and reads the page's own `performance.mark` points plus
`requestAnimationFrame` frame times. Budgets, method and the latest results:
`docs/contributors/dashboard-performance.md`.

```bash
# 1. The stores (deterministic; 60 / 1k / 2k / 10k memories, about 10 s once the example is built)
cargo run --example dashboard_bench_store -- /tmp/dash-bench

# 2. A release binary (the budgets are release figures)
cargo build --release --bin engramdb

# 3. The driver, pinned in bench/browser/package.json (playwright-core: no
#    browser download), and a browser: either pass --chrome <path> to a system
#    Chrome, or install Playwright's Chromium once.
cd crates/engram-dashboard
(cd bench/browser && npm ci && npx playwright-core install chromium)

# 4. The bench
node bench/browser/run.mjs --stores /tmp/dash-bench \
  --bin ../../target/release/engramdb --json /tmp/dash-bench/results.json
```

Options: `--sizes s60,s1k,s2k,s10k`, `--styles plain,grid`,
`--themes light,dark`, `--widths 1280,390`, `--runs 5` (cold page loads per
figure), `--scope storage` (the scope opened from the overview),
`--assets web` (serve the frontend from disk, so a JS or CSS change is
measured without rebuilding the binary), `--chrome /usr/bin/google-chrome`
(a system Chrome instead of Playwright's Chromium), `--port 7400` (the first
port; one per store).

`--paths-only` times only the tab click, the opened scope and show all
(no interactive figures, no relations tab); `--no-layout-cache` starts the dashboard with
`ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0`, so every server layout is cold (the
layout cache otherwise lives under `<stores>/cache`, never the user's).
`--cpu-throttle 4` slows the page's CPU by that factor (Chrome's
`Emulation.setCPUThrottlingRate`), for figures that sit at the 60 fps cap on
a fast machine: zoom on the 10k store's opened scope is 59–60 fps either
way unthrottled, and 23 → 59 at 4× with the composited zoom gesture.

## What each column is

Each JSON line is one (store, style, theme, width). Times are milliseconds,
medians over `--runs` fresh browser contexts (no layout cache) unless noted.
"Painted" means the first frame after the DOM was built: the page's mark,
then `requestAnimationFrame`, then one task.

| field | from → to |
|---|---|
| `tab_click_ms` | click on the Graph tab → painted (`graph:built`, or `graph:overview-built` past 150 memories) |
| `graph_response_ms` | the same click → `graph:data` (the `/graph` fetch and JSON parse) |
| `overview_paint_ms` | `tab_click_ms` when the first view is the overview |
| `open_scope_ms` | click on the `--scope` disc → painted, split into `open_scope_response_ms` (→ `graph:data`), `open_scope_layout_ms` (decoding the server's layout, → `graph:layout`) and `open_scope_build_ms` (the DOM, → `graph:built`); the rest is style, layout and paint |
| `show_all_ms` | `#…/graph?all=1` → painted (the full graph, up to the server's 800-memory limit); `show_all_layout_ms` its layout |
| `tab_segments`, `open_scope_segments`, `show_all_segments` | where the time went, medians, between consecutive marks: `response` (→ `graph:data`), `layout` (→ `graph:layout`: decoding the server's layout), `backdrop`, `nodes`, `mount`, `view`, `legend`, `edges`, `panels` (→ `graph:built`: notes, table, card), `paint` (→ the frame after); the overview's are `response`, `layout`, `build`, `paint` |
| `*_layout_source` | where the layout came from: always `server` (the page has no layout code) |
| `relations_first_page_ms` | click on the Relations tab → painted (`relations:painted`) |
| `filter_toggle_ms` | the Proposed chip → painted, median of 10 (once, on the opened scope or the whole graph) |
| `hover_ms_p50/p95` | `mouseover` (bubbling, as a pointer fires it) on a memory → painted, over 60 memories: latency, so it includes the wait for the next vsync (up to one 16.7 ms frame) |
| `hover_frame_ms_p50/p95` | the hover's own frame: events dispatched at the start of a frame (in a rAF callback) → the task after that frame's paint, over 60 more memories; the main-thread work the 16 ms budget is about |
| `frame_ms_p95/p99` | every frame's length while those hovers run |
| `zoom_fps` | one wheel event per frame, 40 in and 40 out |
| `pan_fps` | a 60-step mouse drag (real input through CDP) |

The marks the page sets: `graph:data`, `graph:layout`, `graph:built`,
`graph:overview-built` (views/graph.js) and `relations:painted`
(views/relations.js). Nothing reads them outside this bench.

The numbers depend on the machine; compare runs on one machine only, and
close other heavy work first (a parallel `cargo build` doubles every figure).

Both graph surfaces are measured the same way: a canvas-drawn style
(Plain) has no element per memory or relation, so the hover figures move
the pointer (`pointermove` at the memory's centre, counting the CPU hit
test) where the SVG one gets `mouseout` / `mouseover`, only memories inside
the box are hovered on either, the pan drags across `.graph-box`, and
`shown_relations` on a canvas is the card's count. `surface` says which.

## `views.mjs`: one view, both surfaces

Holds a view fixed (`--views s1k:all,s2k:all,s10k:open,s10k:all`; `all` is
show all, `open` the largest scope) and times what the surface costs, so two
versions of `web/` can be compared on one binary (`--assets`, `--label`):

```bash
node bench/browser/views.mjs --stores /tmp/dash-bench \
  --assets web --label after --json /tmp/dash-bench/views-after.json
```

`--modes cold,cached` (cold: the server's layout cache off; cached: one
untimed load first), `--runs 5`, `--style plain`, `--theme light`,
`--cpu-throttle 4`, `--dpr 2` (device scale factor), `--gpu` (SwiftShader
as the GPU: headless Chromium otherwise rasters a 2D canvas on the main
thread, inside the paint, which a machine with a GPU does not),
`--paint-only`. Fields (medians over the runs): `paint` (hash change →
the frame after `graph:built`), `draw` (`graph:layout` → that frame),
`settle_ms` (a wheel zoom's 160 ms settle timer → the frame after the last
settle step, the SVG's second timer included) and `settle_work_ms` (their
script time), `zoom_fps`, `pan_fps`, `hover_frame_ms` (p50 of 30), and
`heap_mb` / `dom_nodes` / `layout_objects` after a forced GC.

## `checks.mjs`: interaction and accessibility, both surfaces

Not a timing: pass / fail per view and style, exit 1 on any failure.

```bash
node bench/browser/checks.mjs --stores /tmp/dash-bench --assets web
```

Per view (`s60:full`, `s1k:all`, `s10k:open`) and style (`plain` on the
canvas, `grid` on SVG): one accessible element per drawn memory, in index
order, named `… (<type>, N relations)`, matching the card's count; the
canvas has `role="img"` and a label; `]` / `[` walk memories in order with
roving focus, Enter selects (card heading), a panel row is announced,
Escape clears; hovering 30 memories at their centres hits each one (the
tooltip names it, the cursor is a pointer); a click opens the panel; a
relation pinned from the table is centred and the pointer there hits it;
a display filter moves no memory; a zoom gesture settles; a light / dark
switch repaints the canvas; no console error or CSP violation; and the
canvas lists the same memories, with the same words, as the SVG surface.

## In CI

`ci.yml`'s `frontend-browser` job runs `checks.mjs` on every pull request:

```bash
(cd bench/browser && npm ci --ignore-scripts)
node bench/browser/checks.mjs --stores "$STORES" --bin "$BIN" \
  --chrome "$(command -v google-chrome)" \
  --views s60:full,s1k:all,s1k:open
```

- The binary is the debug build of the `test-load-dynamic-build` job, and the
  stores (`s60`, `s1k`) are generated in the job by the `dashboard_bench_store`
  example built there. `s1k` is above the overview threshold (150), so the
  opened-scope checks run without the 10k store.
- The browser is the runner's Google Chrome (`--chrome`), so nothing is
  downloaded. Playwright starts it without Chrome's sandbox, as it does its
  own Chromium.
- The dashboard declares its icon (`favicon.svg`, linked from `index.html`),
  so a full Chrome no longer asks for `/favicon.ico` and logs a 404; the
  `--allow-missing-favicon` flag that dropped that one console error is gone.
- The job is **not** part of `CI result` yet: the pixel checks (filters undo
  < 0.05 %, copies < 0.2 %, theme round trip identical) have only been run on
  developer machines. Six views pass with a debug binary and Chrome in about
  a minute locally. Once it has been green on ten consecutive master runs, add
  `frontend-browser` to `ci-result.needs`. If a pixel check proves unstable on
  the runner, change its tolerance here with the measurement written down;
  do not delete the check.
- A run on a machine with a full disk fails in `page.goto` ("browser has been
  closed"): Chrome cannot create its profile. That is the environment, not the
  page.
