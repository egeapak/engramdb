// What a drawn memory says to assistive technology, shared by both graph
// surfaces (./surface-svg.js puts it on the memory's `<g>`, the canvas
// host on the memory's button in its accessible list), so a screen reader
// hears the same words whichever one draws.

import { plural } from '../format.js';
import { TYPE_LABEL } from '../scopes.js';

/** A memory's accessible name: its summary, type and relation count. */
export function nodeAria(n) {
  return `${n.summary} (${TYPE_LABEL[n.type] || n.type}, ${plural(n.degree, 'relation')})`;
}
