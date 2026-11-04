// The graph styles and which one is picked.
//
// A style is a module under themes/<id>/index.js exporting the interface
// the core graph view calls (a Rust test checks every one does): `id`,
// `name`, `strings`, `layout`, `drawBackdrop`, `drawNode`, `drawEdge`,
// `edgeSample` and `legend`. The core keeps data, filters, the URL hash,
// selection, keyboard, the details card, the relations table and review.
//
// Precedence: the graph hash's `style=` (so a link shows what its author
// saw), then this browser's choice in localStorage `engramdb.graphStyle`,
// then the default. The pick is page-wide: `data-graph-style` on <html>
// scopes each style's CSS (`data-theme` is already light/dark), and the
// relation samples on other pages and the header's light/dark label follow
// it. A `style=` in a graph link applies while that graph is shown; other
// views go back to the stored choice.

import * as grid from './grid/index.js';
import * as plain from './plain/index.js';

export const STYLES = [grid, plain];

/** Grid for now: phase C gives Plain its own layout and makes it the default. */
export const DEFAULT_STYLE = 'grid';

export const STYLE_KEY = 'engramdb.graphStyle';

export function styleById(id) {
  return STYLES.find((st) => st.id === id) || null;
}

/** This browser's saved pick, or null (none, unknown, or storage refused). */
export function storedStyle() {
  try {
    const v = localStorage.getItem(STYLE_KEY);
    return styleById(v) ? v : null;
  } catch (_) {
    return null;
  }
}

export function storeStyle(id) {
  try {
    localStorage.setItem(STYLE_KEY, id);
  } catch (_) {
    /* private mode: the pick still holds for this page through the hash */
  }
}

let current = null;
const listeners = [];

export function currentStyle() {
  return styleById(current) || styleById(DEFAULT_STYLE);
}

/** Make `id` the picked style (an unknown or empty id: the stored pick, else the default). */
export function useStyle(id) {
  const next = styleById(id) ? id : storedStyle() || DEFAULT_STYLE;
  document.documentElement.setAttribute('data-graph-style', next);
  if (next !== current) {
    current = next;
    for (const fn of listeners) fn(next);
  }
  return currentStyle();
}

export function onStyleChange(fn) {
  listeners.push(fn);
}
