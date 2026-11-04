# Dashboard browser bench

A developer tool, run by hand: it is outside `web/` (never embedded, never
scanned by the frontend tests) and no CI job runs it. It starts
`engramdb dashboard --no-open` on each benchmark store, drives Chromium with
Playwright and reads the page's own `performance.mark` points plus
`requestAnimationFrame` frame times. Budgets, method and the latest results:
`docs/contributors/dashboard-performance.md`.

```bash
# 1. The stores (deterministic; 60 / 1k / 2k / 10k memories, about 10 s once the example is built)
cargo run --example dashboard_bench_store -- /tmp/dash-bench

# 2. A release binary (the budgets are release figures)
cargo build --release --bin engramdb

# 3. The bench (Playwright installed globally: npm i -g playwright && npx playwright install chromium)
cd crates/engram-dashboard
NODE_PATH=$(npm root -g) node bench/browser/run.mjs --stores /tmp/dash-bench \
  --bin ../../target/release/engramdb --json /tmp/dash-bench/results.json
```

Options: `--sizes s60,s1k,s2k,s10k`, `--styles plain,grid`,
`--themes light,dark`, `--widths 1280,390`, `--runs 5` (cold page loads per
figure), `--scope storage` (the scope opened from the overview),
`--assets web` (serve the frontend from disk, so a JS or CSS change is
measured without rebuilding the binary), `--chrome /usr/bin/google-chrome`
(a system Chrome instead of Playwright's Chromium), `--port 7400` (the first
port; one per store).

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
| `open_scope_ms` | click on the `--scope` disc → painted, split into `open_scope_response_ms` (→ `graph:data`), `open_scope_layout_ms` (the worker layout, → `graph:layout`) and `open_scope_build_ms` (the DOM, → `graph:built`); the rest is style, layout and paint |
| `show_all_ms` | `#…/graph?all=1` → painted (the full graph, up to the server's 800-memory limit); `show_all_layout_ms` its layout |
| `relations_first_page_ms` | click on the Relations tab → painted (`relations:painted`) |
| `filter_toggle_ms` | the Proposed chip → painted, median of 10 (once, on the opened scope or the whole graph) |
| `hover_ms_p50/p95` | `mouseenter` on a memory → painted, over 60 memories: latency, so it includes the wait for the next vsync (up to one 16.7 ms frame) |
| `hover_frame_ms_p50/p95` | the hover's own frame: events dispatched at the start of a frame (in a rAF callback) → the task after that frame's paint, over 60 more memories; the main-thread work the 16 ms budget is about |
| `frame_ms_p95/p99` | every frame's length while those hovers run |
| `zoom_fps` | one wheel event per frame, 40 in and 40 out |
| `pan_fps` | a 60-step mouse drag (real input through CDP) |

The marks the page sets: `graph:data`, `graph:layout`, `graph:built`,
`graph:overview-built` (views/graph.js) and `relations:painted`
(views/relations.js). Nothing reads them outside this bench.

The numbers depend on the machine; compare runs on one machine only, and
close other heavy work first (a parallel `cargo build` doubles every figure).
