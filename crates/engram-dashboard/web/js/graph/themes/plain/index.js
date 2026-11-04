// Plain: the neutral graph style. STUB — selectable and sane, nothing more:
// it draws the Grid layout's positions and relation paths with no scope
// chains or badges, memories filled with their scope's colour (shape still
// follows type), and borrows Grid's relation glyphs and legend. Phase C
// gives it its own scope-aware force layout; phase D its own relation
// drawing (tapered strokes, no arrowheads) and legend.

import { s } from '../../../dom.js';
import { slot, typeShape } from '../../../scopes.js';
import * as grid from '../grid/index.js';
import { buildLegend } from '../grid/legend.js';

export const id = 'plain';
export const name = 'Plain';

export const strings = {};

/** Phase C: its own layout (`force-scoped`). Until then, the Grid layout's output. */
export const layout = { name: 'grid' };

/** Nothing behind the memories: no scope chains, no badges. */
export function drawBackdrop() {
  return { below: null, above: null };
}

export function drawNode(n, r) {
  return {
    cls: ['pl', 'fill-' + slot(n.color_slot)],
    parts: [n.challenged ? s('circle', { class: 'chal-ring', r: r + 3.5 }) : null, typeShape(n.type, r)],
  };
}

export const drawEdge = grid.drawEdge;
export const edgeSample = grid.edgeSample;

function legendNode(kind, type) {
  return s(
    'svg',
    { width: 18, height: 18, viewBox: '-9 -9 18 18', 'aria-hidden': 'true', class: 'lg-node' },
    s('g', { class: ['node', 'pl', 'fill-0', kind] }, kind === 'chal' ? s('circle', { class: 'chal-ring', r: 7.5 }) : null, type ? typeShape(type, 4.6) : s('circle', { class: 'sh', r: 5 })),
  );
}

export function legend(g, t) {
  return buildLegend(g, t, {
    scopeSample: (l) => s('svg', { width: 14, height: 14, viewBox: '-7 -7 14 14', 'aria-hidden': 'true' }, s('circle', { class: 'fill-' + slot(l.slot), r: 5 })),
    nodeSample: legendNode,
  });
}
