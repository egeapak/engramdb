// The graph layouts a style can name. A style says which one it uses
// (`layout: { name, options }` in its module); every layout is computed by
// the dashboard server (src/layout/ in the dashboard crate) and sent with
// /graph as `layout`, which graph/server-layout.js decodes. The page asks
// for its style's with `layout=<name>` and has no layout code of its own:
// the server is the only source, and a layout takes no style options. A
// Rust test checks every style's `layout.name` is a key here and the
// server's `LayoutName` accepts it.
//
// The scope overview and an opened scope are not styles' layouts: the core
// view (views/graph.js, views/graph-overview.js) asks the server for them
// (`scope-overview`, `opened`: scope-force by sub-scope) whatever the style.
//
// **Output** — every layout gives the view all of these, in the order of
// the response's `nodes` and `edges`:
//   pos:    [x0, y0, x1, y1, …], one point per node.
//   routes: one polyline [[x, y], …] per edge, stubs included, at least two
//           points; the core places tooltips, pins and the click target
//           along it.
//   labels: one { x, y, anchor, placed } per node. `placed: false` is a
//           fallback position with no free spot: drawn hidden, shown while
//           its memory is highlighted.
//   bounds: { x, y, w, h } covering everything drawn (Fit uses it).
// Any other field (Grid: `chains`; scope-force: `scopes`, `region_r`,
// `groups`) passes through untouched to the style's `drawBackdrop(g, lay)`.

/** Every layout a style can name, each decoded from the server's answer. */
export const LAYOUTS = Object.freeze({ 'scope-force': true, grid: true });
