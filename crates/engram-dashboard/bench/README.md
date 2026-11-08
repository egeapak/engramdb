# Graph layout bench

A developer tool for the dashboard's graph layouts. Every layout is computed
by the server (`src/layout/`: scope-force, Grid's, the scope overview); the
page draws what `/graph` sends and has no layout code of its own. This
folder lives outside `web/`, so `build.rs` never embeds it in the binary and
the frontend tests (wording, imports, CSP) never scan it. Needs Node 22 or
newer (the web modules are plain ES modules; Node detects that on its own).

```bash
cd crates/engram-dashboard
node bench/layout.mjs --url '<token URL of a running dashboard>'   # every project: scope-force and grid
node bench/layout.mjs --url '…' --projects ID --box 1.6x360        # one project, a phone's box
node bench/layout.mjs --graph saved-graph.json                     # a saved /graph response that carries a layout
node bench/layout.mjs                                              # the prototype baseline on n = 60, 1000, 2000, 300x8000, 2000x10000
node bench/layout.mjs --json                                       # one JSON row per (layout, graph)
node bench/details.mjs                                             # the details panel's direction words from both ends (exits 1 on a wrong one)
```

Each row reports:

| column | meaning |
|---|---|
| crossings | proper crossings between straight relations |
| edge/NN | mean relation length / median nearest-neighbour gap, after fitting to 1240 × 780 |
| overlap % | memories under 6 px from another at that fit |
| same scope % | share of each memory's 5 nearest neighbours in its top scope |
| labels placed | labels that found a free spot (the rest show only on hover) |
| contract | the output contract in `web/js/graph/layouts.js` (`pos`, `routes`, `labels`, `bounds`), decoded as the page decodes it |
| same again | (`--url`) a second request gave the same layout |
| ms | (`--url`) the `/graph` response time with the layout, and without `layout=` |

Exits 1 when a layout breaks the contract or differs on the second request.
The server's own compute time is the Criterion bench's
(`benches/dashboard_graph.rs`, the `*_layout` cases); start the dashboard
with `ENGRAMDB_DASHBOARD_LAYOUT_CACHE=0` for the cold `ms` here.

Files:

- `layout.mjs`: the harness. `fromApi(json)` turns a `/graph` response into
  the metrics' graph.
- `data.mjs`: the deterministic synthetic generator (about ten top scopes,
  nested sub-scopes, skewed sizes, 0.7 relations per memory, mostly inside a
  scope), for the prototype baseline.
- `metrics.mjs`: the quality metrics.
- `browser/`: the Playwright bench of the real page (`browser/README.md`).
- `prototype.mjs`, `prototype-common.mjs`: the reviewer's `force-scoped`
  prototype, kept as a baseline only.

To save a real store's graph with its layout, use the dashboard's token
(shown with `engramdb dashboard --no-open`):

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  "http://127.0.0.1:$PORT/api/projects/$ID/graph?limit=2000&layout=scope-force&layout_aspect=0.6&layout_width=1280" > saved-graph.json
```

## Layout tests

The layouts' properties are Rust tests (`src/layout/tests.rs`): the output
contract (finite numbers, routes from their memories, placed labels never
overlapping, everything inside the bounds), determinism (the same input
gives the same bytes, also through the cache's binary form), overview discs
never overlapping, and a layout that is not finite being an error, never
sent. They run on generated graphs and on three `/graph`-shaped inputs in
`src/layout/fixtures/` (long and non-ASCII scope names and summaries,
stubs, one scope by sub-scope at phone width, a 14-scope overview). There
is no JS copy to compare against any more.

## Relation geometry check

`node bench/relations.mjs` imports the Plain style's relation drawing
(`web/js/graph/themes/plain/relations.js`) and checks, over every kind
(unknown names and inherited-property names such as `constructor`
included), status, length and zoom unit: no NaN / Infinity / undefined in
the path data, directed kinds wide at the source, the chevron pointing to
the target, path data bounded per relation, and an inherited-property kind
drawn like an unknown one. It then times building 2,000 relations at Fit and
at maximum zoom against the 100 ms filter-toggle budget (JS share only).
Exits 1 on a failed check.
