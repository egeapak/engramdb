# Graph layout bench

A developer tool for the dashboard's graph layouts (`web/js/graph/layouts.js`).
It lives outside `web/`, so `build.rs` never embeds it in the binary and the
frontend tests (wording, imports, CSP) never scan it. Needs Node 22 or newer
(the web modules are plain ES modules; Node detects that on its own).

```bash
cd crates/engram-dashboard
node bench/layout.mjs                                  # n = 60, 1000, 2000; every layout
node bench/layout.mjs --n 1000 --layouts scope-force --runs 9 --cold 5
node bench/layout.mjs --graph saved-graph.json         # a real store (see below)
node bench/layout.mjs --json                           # one JSON row per (layout, n)
LAYOUT_OPTIONS='{"topPull":0.03}' node bench/layout.mjs --layouts scope-force
```

Each row reports:

| column | meaning |
|---|---|
| cold ms | the first call in a fresh Node process (median of `--cold` processes): roughly what the page's worker pays |
| warm ms | median of `--runs` calls after a warm-up |
| crossings | proper crossings between straight relations |
| edge/NN | mean relation length / median nearest-neighbour gap, after fitting to 1240 × 780 |
| overlap % | memories under 6 px from another at that fit |
| same scope % | share of each memory's 5 nearest neighbours in its top scope |
| labels placed | labels that found a free spot (the rest show only on hover) |
| deterministic | a second run gave identical positions and routes |
| contract | the output contract in `layouts.js` (`pos`, `routes`, `labels`, `bounds`) |

Files:

- `layout.mjs`: the harness. `toInput(graph, layout)` builds the same input
  `views/graph.js` does; `fromApi(json)` turns a saved
  `GET /api/projects/{id}/graph?limit=2000` response into a graph.
- `data.mjs`: the deterministic synthetic generator (about ten top scopes,
  nested sub-scopes, skewed sizes, 0.7 relations per memory, mostly inside a
  scope).
- `metrics.mjs`: the quality metrics.
- `prototype.mjs`, `prototype-common.mjs`: the reviewer's `force-scoped`
  prototype, kept as a baseline only.

To bench a real store, save its graph with the dashboard's token (shown with
`engramdb dashboard --no-open`):

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:$PORT/api/projects/$ID/graph?limit=2000" > saved-graph.json
```

Node is a proxy for the browser. To time the real thing, post the same input
to `js/graph/layout-worker.js` in a fresh Chromium worker (Playwright), once
with a one-memory input first so the timing excludes module loading.
