// Every layout a graph style can name, by name. A style says which one it
// uses (`layout.name` in its module) and the input carries that name, so
// the worker can run it: a function cannot be posted to a worker, and a
// fixed table (rather than an `import()` of a computed URL) keeps every
// layout module visible to the embedded-import check.

import { layout as grid } from './themes/grid/layout.js';

export const LAYOUTS = { grid };

/** Run the layout `input.layout` names (Grid's when it names none). */
export function runNamedLayout(input) {
  const fn = LAYOUTS[input.layout] || grid;
  return fn(input);
}
