// Plain: the neutral graph style, and the default. Memories are placed by
// the core's scope-aware force layout (graph/scope-force.js): no grid, no
// scope chains, no badges; relations are straight. Each top scope is a soft
// region in its colour with one name, and memories keep their scope's
// colour as a fill for now (shape follows type). Relations are drawn with a
// copy of Grid's glyphs (./glyphs.js) until phase D gives Plain its own
// (tapered strokes, no arrowheads).

import { s } from '../../../dom.js';
import { plural } from '../../../format.js';
import { slot, typeShape } from '../../../scopes.js';
import { DIRECTED } from '../../../relations.js';
import { pathD, along, offset } from '../../geometry.js';
import { buildLegend } from '../../legend.js';
import { radius } from '../../radius.js';
import { chevron, terminal, warnSign, stubCap, stubSample, edgeSample as sample } from './glyphs.js';

export const id = 'plain';
export const name = 'Plain';

export const strings = {
  // The glyphs are Grid's for now (phase D), so is their wording.
  legendProposedContradiction: 'Proposed contradiction (dashed, ⚠)',
  noteDangling: (n) => `${plural(n, 'relation')} to a deleted memory (end marked ⊗)`,
  noteUnknown: (n) => `${plural(n, 'relation')} into a store that cannot be checked here (drawn to “?”)`,
};

/** The core's scope-aware force layout (graph/scope-force.js), default options. */
export const layout = { name: 'scope-force', options: {} };

/**
 * Under the relations, each named top scope's soft region: one path per
 * scope, the union of a disc around each of its memories (`lay.region_r`;
 * one path, so overlapping discs never darken). Over the relations, one name
 * per top scope where the layout found it a free spot (`lay.scopes`).
 */
export function drawBackdrop(g, lay) {
  const rr = lay.region_r || 24;
  const arcs = new Map();
  g.nodes.forEach((n, i) => {
    if (!n.top_scope) return;
    const x = lay.pos[2 * i];
    const y = lay.pos[2 * i + 1];
    // At least the memory's own disc and outline, so a large one is never
    // wider than its region.
    const q = Math.max(radius(n) + 6, rr);
    const qs = q.toFixed(1);
    const d = `M${(x - q).toFixed(1)},${y.toFixed(1)}a${qs},${qs} 0 1,0 ${(2 * q).toFixed(1)},0a${qs},${qs} 0 1,0 ${(-2 * q).toFixed(1)},0`;
    if (!arcs.has(n.top_scope)) arcs.set(n.top_scope, { slot: n.color_slot, d: [] });
    arcs.get(n.top_scope).d.push(d);
  });
  const below = s('g', { class: 'scope-regions', 'aria-hidden': 'true' });
  for (const [, a] of [...arcs].sort((p, q) => (p[0] < q[0] ? -1 : p[0] > q[0] ? 1 : 0))) {
    below.appendChild(s('path', { class: ['region', 'fill-' + slot(a.slot)], d: a.d.join('') }));
  }
  const above = s('g', { class: 'scope-names', 'aria-hidden': 'true' });
  for (const nm of lay.scopes || []) {
    if (!nm.placed) continue;
    // A name over memories gets a faint backing box, so it reads between them.
    if (nm.backed) {
      const w = nm.w || nm.key.length * nm.size * 0.62;
      above.appendChild(s('rect', { class: 'scope-name-bg', x: (nm.x - w / 2).toFixed(1), y: (nm.y - nm.size * 0.96).toFixed(1), width: w.toFixed(1), height: (nm.size * 1.2).toFixed(1), rx: (nm.size * 0.3).toFixed(1) }));
    }
    above.appendChild(s('text', { class: 'scope-name', x: nm.x.toFixed(1), y: nm.y.toFixed(1), 'font-size': nm.size.toFixed(1), 'text-anchor': 'middle' }, nm.key));
  }
  return { below, above };
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
