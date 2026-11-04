// Plain: the neutral graph style. STUB — selectable and sane, nothing more:
// it draws the Grid layout's positions and relation paths (through the
// core's layout table) with no scope chains or badges, memories filled with
// their scope's colour (shape still follows type), and relations with a copy
// of Grid's glyphs (./glyphs.js). Phase C gives it its own scope-aware force
// layout; phase D its own relation drawing (tapered strokes, no arrowheads).

import { s } from '../../../dom.js';
import { plural } from '../../../format.js';
import { slot, typeShape } from '../../../scopes.js';
import { DIRECTED } from '../../../relations.js';
import { pathD, along, offset } from '../../geometry.js';
import { buildLegend } from '../../legend.js';
import { chevron, terminal, warnSign, stubCap, stubSample, edgeSample as sample } from './glyphs.js';

export const id = 'plain';
export const name = 'Plain';

export const strings = {
  // The glyphs are Grid's for now (phase D), so is their wording.
  legendProposedContradiction: 'Proposed contradiction (dashed, ⚠)',
  noteDangling: (n) => `${plural(n, 'relation')} to a deleted memory (end marked ⊗)`,
  noteUnknown: (n) => `${plural(n, 'relation')} into a store that cannot be checked here (drawn to “?”)`,
};

/** Phase C: its own layout (`force-scoped`). Until then, the Grid layout's output. */
export const layout = { name: 'grid', options: {} };

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

/** Today's relation drawing along the layout's path (see ./glyphs.js). */
export function drawEdge(e, pts, ends) {
  const d = pathD(pts);
  const parts = [s('path', { class: 'halo', d }), s('path', { class: 'cn', d })];
  if (e.kind === 'contradicts') {
    const a = offset(pts, ends.rs);
    parts.push(terminal(a.x, a.y, a.ang));
    if (!ends.stub) {
      const b = offset(pts, ends.rt, true);
      parts.push(terminal(b.x, b.y, b.ang));
    }
    if (e.status === 'proposed') {
      const m = along(pts, 0.5);
      parts.push(warnSign(m.x, m.y, 0.85));
    }
  } else if (DIRECTED[e.kind]) {
    const b = offset(pts, ends.rt + 2, true);
    parts.push(chevron(b.x, b.y, b.ang + Math.PI));
  }
  if (ends.stub) {
    const end = pts[pts.length - 1];
    parts.push(stubCap(end[0], end[1], e.state === 'unknown' ? 'unknown' : 'dangling'));
  }
  return { cls: [], parts };
}

export const edgeSample = sample;

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
    edgeSample: sample,
    stubSample,
  });
}
