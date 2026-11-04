// Every layout a graph style can name, by name. A style says which one it
// uses (`layout: { name, options }` in its module) and the input carries
// both, so the worker can run it: a function cannot be posted to a worker,
// and a fixed table (rather than an `import()` of a computed URL) keeps
// every layout module visible to the embedded-import check. A Rust test
// checks every style's `layout.name` is a key here.
//
// Phase E's scope overview (one disc per scope, bands between them) is not a
// style: it will be a separate core render path with its own entry in this
// table, chosen by the core view (views/graph.js says where).
//
// **Input** (built by views/graph.js):
//   { layout: name, options: the style's layout options (or {}),
//     nodes: [{ id, top_scope, slot, scope, degree, r, label, weight }],
//     edges: [{ s, t, kind, far_scope }], aspect: box height / width,
//     width: the box's width in CSS pixels, rounded to 40 }
//   `t` < 0 is a stub (dangling or cross-store) drawn off `s`; `far_scope`
//   is the far end's top scope ('' for none, null when unknown, as for every
//   stub today).
//
// **Output** — every layout must return all of these:
//   pos:    [x0, y0, x1, y1, …], one point per node.
//   routes: one polyline [[x, y], …] per edge, stubs included, at least two
//           points (a straight layout returns [source, target]); the core
//           places tooltips, pins and the click target along it.
//   labels: one { x, y, w, anchor, placed } per node, never null (use
//           graph/labels.js). `placed: false` is a fallback position with no
//           free spot: drawn hidden, shown while its memory is highlighted.
//   bounds: { x, y, w, h } covering everything drawn (Fit uses it).
// Any other field (Grid: `lines`) passes through untouched to the style's
// `drawBackdrop(g, lay)`.

import { layout as grid } from './themes/grid/layout.js';
import { layout as scopeForce } from './scope-force.js';

export const LAYOUTS = Object.freeze({ grid, 'scope-force': scopeForce });

/** Run the layout `input.layout` names; an unknown name is an error, never a silent fallback. */
export function runNamedLayout(input) {
  if (typeof input.layout !== 'string' || !Object.hasOwn(LAYOUTS, input.layout)) {
    throw new Error(`unknown graph layout ${JSON.stringify(input.layout)}`);
  }
  return LAYOUTS[input.layout](input);
}
